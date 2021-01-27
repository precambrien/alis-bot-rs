use clap::{App, AppSettings, Arg};
use failure::Error;
use glob::Pattern;
use irc::client::prelude::*;
use log::{debug};
use std::fmt;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};
use std::thread;
#[macro_use]
extern crate failure;

// bot command options
const HELP_COMMAND: &str = "help";
const LIST_COMMAND: &str = "list";
const OPT_CHAN_PATTERN: &str = "pattern";
const OPT_TOPIC_PATTERN: &str = "topic";
const OPT_TOPIC_PATTERN_SHORT: char = 't';
const OPT_FORCE_UPDATE_SHORT: char = 'f';
const OPT_FORCE_UPDATE: &str = "force";
const OPT_MAX_USERS: &str = "max";
const OPT_MIN_USERS: &str = "min";
// bot configuration
const LIST_CACHE_TIME_SECS: u64 = 300; /* server list is cached for 5 min */
// misc
const IRC_EOL: &str = "\r\n";

const USAGE_LIST: &'static str = "
list-alis-bot-rs -- allows searching for channels with more flexibility than the /list command.
Usage:
  list <pattern> [OPTIONS]		shows a list of channels matching the pattern
Arguments:
  <pattern>					channel \x02name\x0f matches <pattern> (Unix shell style glob pattern)
Options:
  -t --topic <pattern>		channel \x02topic\x0f matches <pattern> (Unix shell style glob pattern)
  --max <n>					shows only channels with \x02at most\x0f <n> users
  --min <n>					shows only channels with \x02at least\x0f <n> users
  -f 						forces channel list update. By default, channel list is cached and expires after 5 minutes
 Examples:
 /msg alis-bot-rs list *searchterm*
 /msg alis-bot-rs list * --topic multiple*ordered*search*terms
 /msg alis-bot-rs list #foo* --min 50
 /msg alis-bot-rs list *bar? -f";

const INTRODUCE: &'static str = "alis-bot-rs allows searching for channels with more flexibility than the /list command. For command syntax type:\r\n/msg alis-bot-rs help\r\n";

#[derive(Debug, PartialEq)]
struct Request {
    chan_pattern: Pattern,
    topic_pattern: Option<Pattern>,
    max_users: Option<u32>,
    min_users: Option<u32>,
    force_update: bool,
}

impl Request {
    fn new(
        chan_pattern: Option<&str>,
        topic_pattern: Option<&str>,
        max_users: Option<&str>,
        min_users: Option<&str>,
        force_update: bool,
    ) -> Result<Request, Error> {
        let chan_pattern = match chan_pattern {
            Some(s) => Pattern::new(&s.to_string()).unwrap(),
            _ => return Err(format_err!("No pattern specified on channel name")),
        };
        let topic_pattern = match topic_pattern {
            Some(s) => Some(Pattern::new(&s.to_string()).unwrap()),
            _ => None,
        };
        let max_users: Option<u32> = match max_users {
            Some(max) => parse_opt_u32(Some(max.to_string()))?,
            _ => None,
        };
        let min_users: Option<u32> = match min_users {
            Some(min) => parse_opt_u32(Some(min.to_string()))?,
            _ => None,
        };

        Ok(Request {
            chan_pattern,
            topic_pattern,
            max_users,
            min_users,
            force_update,
        })
    }

    fn process(
        &self,
        client: &Client,
        mutcond: &Arc<(Mutex<(bool, ChannelListing)>, Condvar)>,
    ) -> (Vec<String>, Duration) {
        let (ref mtx, ref cnd) = &**mutcond;
        let expired;
        {
            let guard = mtx.lock().unwrap();
            let listing = &guard.1;
            expired = listing.has_expired();
        }
        if self.force_update || expired {
            {
                let mut guard = mtx.lock().unwrap();
                /* listing made unavailable from now */
                guard.0 = false;
                let listing = &mut guard.1;
                listing.reset();
                send_list_command(&client);
            }

            let mut guard = mtx.lock().unwrap();
            debug!("Waiting for channel list update...");
            while !guard.0 {
                guard = cnd.wait(guard).unwrap();
            }
        }
        let guard = mtx.lock().unwrap();
        let listing = &guard.1;
        let channels = &listing.channels;
        debug!("Processing request on {} channels", channels.len());

        let result = channels
            .iter()
            .filter(|chan| chan.matches(&self))
            .map(|chan| chan.to_string())
            .collect();
        let elapsed_time = listing.get_elapsed_time();

        (result, elapsed_time)
    }
}
impl fmt::Display for Request {
    fn fmt(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        let topic = if let Some(t) = &self.topic_pattern {
            format!("{}", t)
        } else {
            "(None)".to_string()
        };
        let max = if let Some(m) = &self.max_users {
            format!("{}", m)
        } else {
            "(None)".to_string()
        };
        let min = if let Some(m) = &self.min_users {
            format!("{}", m)
        } else {
            "(None)".to_string()
        };
        write!(
            formatter,
            "(channel name pattern: {}, topic pattern: {}, max users: {}, min users: {})",
            self.chan_pattern, topic, max, min
        )
    }
}

pub fn privmsg_parse(
    client: &Client,
    mutcond: &Arc<(Mutex<(bool, ChannelListing)>, Condvar)>,
    source: &str,
    msg: &String,
) -> () {
    let msg = msg.to_lowercase();
    let vec: Vec<&str> = msg.split_whitespace().collect();

    match vec[0] {
        LIST_COMMAND => {
            let request = match get_request_from_args(vec) {
                Ok(req) => req,
                Err(_e) => {
                    client.send_privmsg(&source, list_usage_msg()).unwrap();
                    return;
                }
            };
            let (result, last_fetch) = request.process(client, mutcond);
            // Delay result to avoid anti-flooding policies 
			for message in &result {
            	client.send_privmsg(&source, message).unwrap();
            	thread::sleep(Duration::from_millis(1000));
           }
           let end_msg = format!("\x02Total: {} channel(s)\x0f matching: '{}'. Last list update was cached {} ago, run with -f to force fetching and get the most up-to-date results.",
                        &result.len(),
                        &request,
                        format_duration(last_fetch)
                    );
            client.send_privmsg(&source, end_msg).unwrap();
            debug!("{} channels matching request", &result.len());
        }
        HELP_COMMAND => client.send_privmsg(&source, list_usage_msg()).unwrap(),
        _ => client.send_privmsg(&source, introduce_msg(source)).unwrap(),
    };
}

fn list_usage_msg() -> String {
    USAGE_LIST.replace('\n', IRC_EOL)
}

fn introduce_msg(user_name: &str) -> String {
    format!("Hey {} ! {}", user_name, INTRODUCE)
}

pub struct ChannelListing {
    channels: Vec<Channel>,
    last_fetch: Instant,
}
impl ChannelListing {
    pub fn new() -> ChannelListing {
        ChannelListing {
            channels: Vec::new(),
            last_fetch: Instant::now(),
        }
    }
    pub fn add_channel(&mut self, v: &Vec<String>) {
        let channels = &mut self.channels;
        if let Ok(channel) = Channel::new(v) {
            channels.push(channel);
        }
    }
    pub fn set_timestamp(&mut self) {
        self.last_fetch = Instant::now();
    }
    pub fn len(&self) -> usize {
        self.channels.len()
    }
    fn has_expired(&self) -> bool {
        let now = Instant::now();
        now.duration_since(self.last_fetch) > Duration::from_secs(LIST_CACHE_TIME_SECS)
    }
    fn reset(&mut self) {
        self.channels = Vec::new();
        self.set_timestamp();
    }
    fn get_elapsed_time(&self) -> Duration {
        Instant::now().duration_since(self.last_fetch)
    }
}

fn build_list_app() -> App<'static> {
    App::new("list-alis-bot-rs")
        .setting(AppSettings::NoBinaryName)
        .version("1.0")
        .about("allows searching for channels with more flexibility than the /list command")
        .subcommand(
            App::new(LIST_COMMAND)
                .about("shows a list of channels matching the pattern")
                .arg(
                    Arg::new(OPT_CHAN_PATTERN)
                        .about("channel name matches pattern")
                        .required(true)
                        .index(1),
                )
                .arg(
                    Arg::new(OPT_TOPIC_PATTERN)
                        .short(OPT_TOPIC_PATTERN_SHORT)
                        .long(OPT_TOPIC_PATTERN)
                        .takes_value(true)
                        .about("channel topic matches pattern"),
                )
                .arg(
                    Arg::new(OPT_MIN_USERS)
                        .long(OPT_MIN_USERS)
                        .takes_value(true)
                        .about("shows only channels with at least <n> users"),
                )
                .arg(
                    Arg::new(OPT_MAX_USERS)
                        .long(OPT_MAX_USERS)
                        .takes_value(true)
                        .about("shows only channels with at most <n> users"),
                )
                .arg(
                    Arg::new(OPT_FORCE_UPDATE)
                        .short(OPT_FORCE_UPDATE_SHORT)
                        .long(OPT_FORCE_UPDATE)
                        .about("force channel list update"),
                ),
        )
}

fn get_request_from_args(args: Vec<&str>) -> Result<Request, Error> {
    let matches = build_list_app().try_get_matches_from(args);
    let m = match matches {
        Err(_e) => return Err(format_err!("Error parsing request")),
        Ok(ref matches) => match matches.subcommand() {
            Some((LIST_COMMAND, list_matches)) => list_matches,
            _ => unreachable!(),
        },
    };

    let request = Request::new(
        m.value_of(OPT_CHAN_PATTERN),
        m.value_of(OPT_TOPIC_PATTERN),
        m.value_of(OPT_MAX_USERS),
        m.value_of(OPT_MIN_USERS),
        m.is_present(OPT_FORCE_UPDATE),
    );
    request
}

struct Channel {
    name: String,
    topic: String,
    users: u32,
}

impl Channel {
    pub fn new(vec: &Vec<String>) -> Result<Channel, Error> {
        match vec.len() {
            4 => {
                let (name, topic, users) = (
                    vec[1].clone(),
                    vec[3].clone(),
                    vec[2].clone().parse::<u32>()?,
                );
                Ok(Channel { name, topic, users })
            }
            _ => Err(format_err!("Cannot parse RPL_LIST response from server")),
        }
    }
    fn matches(&self, request: &Request) -> bool {
        request.chan_pattern.matches(&self.name)
            && match &request.topic_pattern {
                Some(topic_pattern) => topic_pattern.matches(&self.topic),
                None => true,
            }
            && match request.max_users {
                Some(max) => self.users <= max,
                None => true,
            }
            && match request.min_users {
                Some(min) => self.users >= min,
                None => true,
            }
    }
}

impl fmt::Display for Channel {
    fn fmt(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        write!(
            formatter,
            "{: <25} {}: {}",
            self.name, self.users, self.topic
        )
    }
}

pub fn send_list_command(client: &Client) {
    debug!("Channel list request...");
    client.send(Command::LIST(None, None)).unwrap();
}

fn format_duration(duration: Duration) -> String {
    let seconds = duration.as_secs() % 60;
    let minutes = (duration.as_secs() / 60) % 60;
    if minutes > 0 {
        format!("{}min{}s", minutes, seconds)
    } else {
        format!("{}s", seconds)
    }
}

fn parse_opt_u32(arg: Option<String>) -> Result<Option<u32>, Error> {
    match arg {
        Some(arg) => {
            let val = arg.parse::<u32>()?;
            Ok(Some(val))
        }
        _ => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn channel_matches_req() {
        let request = Request::new(Some("?test*"), Some("?bar"), Some("5"), None, false).unwrap();
        let matching_rpl_list = vec![
            String::from("foo"),
            String::from("#test-channel"),
            String::from("4"),
            String::from("abar"),
        ];
        let matching_channel = Channel::new(&matching_rpl_list).unwrap();
        assert_eq!(matching_channel.matches(&request), true);

        let bad_topic_rpl_list = vec![
            String::from(""),
            String::from("#test-channel"),
            String::from("4"),
            String::from("other_topic"),
        ];
        let bad_topic_chan = Channel::new(&bad_topic_rpl_list).unwrap();
        assert_eq!(bad_topic_chan.matches(&request), false);

        let line_request = vec!["list", "*", "--min", "2", "--topic", "two*terms"];
        let request = get_request_from_args(line_request).unwrap();
        let matching_rpl_list = vec![
            String::from("foo"),
            String::from("#test-channel"),
            String::from("2"),
            String::from("twoterms"),
        ];
        let matching_channel = Channel::new(&matching_rpl_list).unwrap();
        assert_eq!(matching_channel.matches(&request), true);

        let line_request = vec!["list", "#test*", "-t", "*two*", "--max", "0"];
        let zero_users_request = get_request_from_args(line_request).unwrap();
        assert_eq!(matching_channel.matches(&zero_users_request), false);
        let line_request = vec!["list", "*", "-t", "*", "--min", "2", "--max", "2"];
        let exact_users_request = get_request_from_args(line_request).unwrap();
        assert_eq!(matching_channel.matches(&exact_users_request), true);
    }
    #[test]
    fn usage_examples() {
        // /msg alis-bot-rs list searchterm
        let line_request = vec!["list", "*searchterm*"];
        let request = get_request_from_args(line_request).unwrap();
        let matching_rpl_list = vec![
            String::from("foo"),
            String::from("#searchterm-channel"),
            String::from("4"),
            String::from("abar"),
        ];
        let matching_channel = Channel::new(&matching_rpl_list).unwrap();
        assert_eq!(matching_channel.matches(&request), true);
        // /msg alis-bot-rs list * --topic multiple*ordered*search*terms
        let line_request = vec!["list", "*", "--topic", "multiple*ordered*search*terms"];
        let request = get_request_from_args(line_request).unwrap();
        let matching_rpl_list = vec![
            String::from("foo"),
            String::from("#some-chan"),
            String::from("4"),
            String::from("multiple-ordered-and-glued-searchterms"),
        ];
        let matching_channel = Channel::new(&matching_rpl_list).unwrap();
        assert_eq!(matching_channel.matches(&request), true);
        // /msg alis-bot-rs list #foo* --min 50
        let line_request = vec!["list", "#foo*", "--min", "50"];
        let request = get_request_from_args(line_request.clone()).unwrap();
        let matching_rpl_list = vec![
            String::from("foo"),
            String::from("#footnote"),
            String::from("50"),
            String::from(""),
        ];
        let matching_channel = Channel::new(&matching_rpl_list).unwrap();
        assert_eq!(matching_channel.matches(&request), true);
        let request = get_request_from_args(line_request).unwrap();
        let matching_rpl_list = vec![
            String::from("foo"),
            String::from("#footnote"),
            String::from("0"),
            String::from(""),
        ];
        let matching_channel = Channel::new(&matching_rpl_list).unwrap();
        assert_eq!(matching_channel.matches(&request), false);
        // /msg alis-bot-rs list *bar? -f
        let line_request = vec!["list", "*bar?", "-f"];
        let request = get_request_from_args(line_request.clone()).unwrap();
        let matching_rpl_list = vec![
            String::from(""),
            String::from("#barx"),
            String::from("5"),
            String::from(""),
        ];
        let matching_channel = Channel::new(&matching_rpl_list).unwrap();
        assert_eq!(matching_channel.matches(&request), true);
    }
    #[test]
    fn parse_failure() {
        let result = parse_opt_u32(Some("string".to_string()));
        assert!(result.is_err());

        let result = parse_opt_u32(Some("-1".to_string()));
        assert!(result.is_err());
    }
    #[test]
    fn parse_success() {
        let result = parse_opt_u32(Some("1".to_string())).unwrap();
        let expected = Some(1);
        assert_eq!(expected, result);

        let result = parse_opt_u32(Some("0".to_string())).unwrap();
        let expected = Some(0);
        assert_eq!(expected, result);

        let result = parse_opt_u32(None).unwrap();
        let expected = None;
        assert_eq!(expected, result);
    }
    #[test]
    fn use_cached_list() {
        let listing = ChannelListing {
            channels: Vec::new(),
            last_fetch: Instant::now(),
        };
        assert_eq!(listing.has_expired(), false);
    }
    #[test]
    fn ask_new_list() {
        let listing = ChannelListing {
            channels: Vec::new(),
            last_fetch: Instant::now() - Duration::from_secs(301),
        };
        assert_eq!(listing.has_expired(), true);
    }
    #[test]
    fn simple_pattern_request() {
        let request = Request {
            chan_pattern: Pattern::new("*test*").unwrap(),
            topic_pattern: None,
            max_users: None,
            min_users: None,
            force_update: false,
        };
        let line_request = vec!["list", "*test*"];
        assert_eq!(get_request_from_args(line_request).unwrap(), request);
    }
    #[test]
    fn full_pattern_request() {
        let request = Request {
            chan_pattern: Pattern::new("*test*").unwrap(),
            topic_pattern: Some(Pattern::new("*other*").unwrap()),
            max_users: None,
            min_users: Some(5),
            force_update: true,
        };
        let line_request = vec!["list", "*test*", "--topic", "*other*", "--min", "5", "-f"];
        assert_eq!(get_request_from_args(line_request).unwrap(), request);
    }
    #[test]
    fn mandatory_channel_pattern() {
        let line_request = vec!["list", "-t", "*test*"];
        assert!(get_request_from_args(line_request).is_err());
    }
    #[test]
    fn mandatory_option_names() {
        let line_request = vec!["list", "*test*", "5"];
        assert!(get_request_from_args(line_request).is_err());
    }
    #[test]
    fn mandatory_opt_values_if_named() {
        let line_request = vec!["list", "*test*", "-t"];
        assert!(get_request_from_args(line_request).is_err());
    }
    #[test]
    fn no_remaining_args_allowed() {
        let line_request = vec!["list", "*test*", "left_alone"];
        assert!(get_request_from_args(line_request).is_err());
    }
    #[test]
    fn shuffled_opts_allowed() {
        let request = Request {
            chan_pattern: Pattern::new("*test*").unwrap(),
            topic_pattern: Some(Pattern::new("*other*").unwrap()),
            max_users: Some(5),
            min_users: Some(2),
            force_update: true,
        };
        let line_request = vec![
            "list", "*test*", "--min=2", "--max=5", "-f", "-t", "*other*",
        ];
        assert_eq!(get_request_from_args(line_request).unwrap(), request);
    }
    #[test]
    fn list_arg_at_first_position() {
        let line_request = vec!["-t", "*other*", "list", "*test*", "--min=2", "--max=5"];
        assert!(get_request_from_args(line_request).is_err());
    }
    #[test]
    fn opts_before_pattern_allowed() {
        let line_request = vec!["list", "-t", "*other*", "-f", "--min=2", "*test*"];
        let request = Request {
            chan_pattern: Pattern::new("*test*").unwrap(),
            topic_pattern: Some(Pattern::new("*other*").unwrap()),
            max_users: None,
            min_users: Some(2),
            force_update: true,
        };
        assert_eq!(get_request_from_args(line_request).unwrap(), request);
    }
    #[test]
    fn other_request_than_list() {
        let line_request = vec!["not", "a", "list", "command"];
        // must return error even if "list" literal is in the message
        assert!(get_request_from_args(line_request).is_err());
    }
}
