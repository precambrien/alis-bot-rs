use alis_bot_rs::*;
use clap::{App, Arg, ArgMatches};
use failure::Error;
use futures::prelude::*;
use glob::glob;
use irc::client::prelude::*;
use log::{debug, error, info};
use std::path::PathBuf;
use std::sync::mpsc::channel;
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use tokio::runtime::Runtime;
#[macro_use]
extern crate failure;

const CONFIG_FILE_OPT: &str = "config";
const CONFIG_DIR_OPT: &str = "conf-dir";
const CONFIG_FILE_EXT: &str = "toml";
const DEFAULT_CONFIG_FILE: &str = "example_config.toml";

fn build_app() -> App<'static> {
    App::new("alis-bot-rs")
        .version("1.0")
        .about("alis-unofficial IRC bot")
        .arg(
            Arg::new("config")
                .about("configuration file(s) to use")
                .takes_value(true)
                .short('c')
                .long("config")
                .value_name("FILE")
                .multiple(true)
                .conflicts_with("conf-dir"),
        )
        .arg("-d, --conf-dir=[DIR] 'configuration directory to use'")
}

fn main() {
    let matches = build_app().get_matches();

    env_logger::init();

    let configs = match get_config_paths_from_cli(matches) {
        Ok(c) => c,
        Err(e) => {
            error!("{}", e);
            match get_config_path_from_default() {
                Ok(c) => c,
                Err(e) => {
                    error!("{}", e);
                    return;
                }
            }
        }
    };

    let rt = Runtime::new().unwrap();
    /* tasked instances */
    rt.block_on(async move {
        for config in configs {
            tokio::spawn(async move { run_instance(&config).await });
        }
    });
    loop {}
}

fn get_config_paths_from_cli(matches: ArgMatches) -> Result<Vec<PathBuf>, Error> {
    let paths: Vec<PathBuf> = {
        if matches.is_present(CONFIG_FILE_OPT) {
            matches
                .values_of(CONFIG_FILE_OPT)
                .unwrap()
                .filter_map(|s| config_file_is_valid(PathBuf::from(s)).ok())
                .collect()
        } else if matches.is_present(CONFIG_DIR_OPT) {
            if let Some(user_glob) = matches.value_of(CONFIG_DIR_OPT) {
                let user_glob = format!("{}/*.{}", user_glob, CONFIG_FILE_EXT);
                glob(&user_glob)
                    .expect("Failed to read glob pattern")
                    .filter_map(|s| s.ok())
                    .filter_map(|s| config_file_is_valid(s).ok())
                    .collect()
            } else {
                return Err(format_err!("No directory value specified"));
            }
        } else {
            return Err(format_err!(
                "No configuration file specified, using default."
            ));
        }
    };
    if paths.len() == 0 {
        return Err(format_err!("No valid configuration files found"));
    }

    Ok(paths)
}

fn config_file_is_valid(path: PathBuf) -> Result<PathBuf, Error> {
    let error;
    if let Ok(config) = Config::load(&path) {
        if let Some(_server) = config.server {
            return Ok(path);
        } else {
            error = format_err!(
                "Configuration file: {}, no server specified",
                path.as_path().display().to_string()
            );
        }
    } else {
        error = format_err!("File not found: {}", path.as_path().display().to_string());
    }
    error!("{}", error);
    Err(error)
}

fn get_config_path_from_default() -> Result<Vec<PathBuf>, Error> {
    let path = match config_file_is_valid(PathBuf::from(DEFAULT_CONFIG_FILE)) {
        Ok(p) => p,
        Err(e) => return Err(e),
    };

    info!(
        "Using default configuration file: {}",
        path.as_path().display().to_string()
    );

    Ok(vec![path])
}

async fn run_instance(config: &PathBuf) -> irc::error::Result<()> {
    let config = Config::load(&config)?;
    let mut client = Client::from_config(config.clone()).await?;
    client.identify()?;
    let mut stream = client.stream()?;
    if let Some(server) = config.server {
        info!("Connected to {}", server);
    }

    let mut server_name: Option<String> = None;
    let listing = ChannelListing::new();

    // private messages mpsc channel
    let (ms, mr) = channel::<Message>();
    // shared client
    let client = Arc::new(client);
    let privmsg_client = Arc::clone(&client);
    // Mutex with condition for listing access
    let mutcond: Arc<(Mutex<(bool, ChannelListing)>, Condvar)> =
        Arc::new((Mutex::new((false, listing)), Condvar::new()));
    let c_mutcond = Arc::clone(&mutcond);

    let privmsg_thread = thread::spawn(move || loop {
        let message = mr.recv().unwrap();
        if let Command::PRIVMSG(_target, msg) = &message.command {
            let source = match message.source_nickname() {
                Some(s) => s,
                None => continue,
            };
            privmsg_parse(&privmsg_client, &c_mutcond, &source, &msg);
        }
    });

    while let Some(message) = stream.next().await.transpose()? {
        match &message.command {
            Command::PRIVMSG(target, _msg) => {
                // responds only to private message, ignoring unspecified source and server messages
                if target.eq(&client.current_nickname()) {
                    let source = if let Some(s) = message.source_nickname() {
                        s
                    } else {
                        continue;
                    };
                    match &server_name {
                        Some(server_name) if source.eq(server_name) => continue,
                        _ => ms.send(message).unwrap(),
                    }
                }
            }
            Command::Response(rpl_type, v) if *rpl_type == Response::RPL_LIST => {
                /* updating channel list */
                let &(ref mtx, ref _cnd) = &*mutcond;
                let mut guard = mtx.lock().unwrap();
                let listing = &mut guard.1;
                listing.add_channel(v);
            }
            Command::Response(rpl_type, _v) if *rpl_type == Response::RPL_LISTEND => {
                let &(ref mtx, ref cnd) = &*mutcond;
                let mut guard = mtx.lock().unwrap();
                let listing = &mut guard.1;
                listing.set_timestamp();
                debug!(
                    "Channel list request...done. {} channels received",
                    &listing.len()
                );
                /* listing made available from now */
                guard.0 = true;
                cnd.notify_all();
            }
            Command::Response(rpl_type, _) if *rpl_type == Response::RPL_WELCOME => {
                if let Some(Prefix::ServerName(name)) = &message.prefix {
                    server_name = Some(name.to_string());
                }
                send_list_command(&client);
            }
            _ => (),
        }
    }

    let _ = privmsg_thread.join();

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::{rename, File};
    use std::io::Write;
    use tempfile::Builder;

    #[test]
    fn conflicting_args() {
        let cmd = ["alis-bot-rs", "-c", "some_file", "-d", "some_dir"].iter();
        let matches = build_app().try_get_matches_from(cmd);
        assert!(matches.is_err());
    }
    #[test]
    fn multiple_files_on_c_option() {
        let mut expected: Vec<_> = Vec::new();
        let dir = Builder::new()
            .prefix("test")
            .rand_bytes(0)
            .tempdir()
            .unwrap();
        for i in 1..3 {
            let file_path = dir.path().join(format! {"{}_file.toml", i});
            let mut file = File::create(&file_path).unwrap();
            writeln!(file, "server = \"test\"").unwrap();
            expected.push(file_path);
        }
        let cmd = [
            "alis-bot-rs",
            "-c",
            "/tmp/test/1_file.toml",
            "/tmp/test/2_file.toml",
        ]
        .iter();
        let matches = build_app().get_matches_from(cmd);
        let result = get_config_paths_from_cli(matches).unwrap();
        assert_eq!(result, expected);
        let unvalid_file = dir.path().join("error_file.toml");
        let _file = File::create(&unvalid_file).unwrap();
        let cmd = [
            "alis-bot-rs",
            "-c",
            "/tmp/test/1_file.toml",
            "/tmp/test/2_file.toml",
            "/tmp/test/error_file.toml",
        ]
        .iter();
        let matches = build_app().get_matches_from(cmd);
        let result = get_config_paths_from_cli(matches).unwrap();
        assert_eq!(result, expected);
    }
    #[test]
    fn multiple_files_in_directory() {
        let mut expected: Vec<_> = Vec::new();
        let dir = Builder::new()
            .prefix("dir")
            .rand_bytes(0)
            .tempdir()
            .unwrap();
        for i in 1..4 {
            let file_path = dir.path().join(format! {"{}_file.toml", i});
            let mut file = File::create(&file_path).unwrap();
            writeln!(file, "server = \"test\"").unwrap();
            expected.push(file_path);
        }
        let cmd = ["alis-bot-rs", "-d", "/tmp/dir"].iter();
        let matches = build_app().get_matches_from(cmd);
        let result = get_config_paths_from_cli(matches).unwrap();
        assert_eq!(result, expected);
    }
    #[test]
    fn directory_failures_errors() {
        let cmd = ["alis-bot-rs", "-d", "/unaccessible/path"].iter();
        let matches = build_app().get_matches_from(cmd);
        assert!(get_config_paths_from_cli(matches).is_err());
        let _dir = Builder::new()
            .prefix("empty")
            .rand_bytes(0)
            .tempdir()
            .unwrap();
        let cmd = ["alis-bot-rs", "-d", "/empty/"].iter();
        let matches = build_app().get_matches_from(cmd);
        assert!(
            get_config_paths_from_cli(matches).is_err(),
            "No valid configuration files found"
        );
    }
    #[test]
    fn use_default_config() {
        let cmd = ["alis-bot-rs"].iter();
        let matches = build_app().get_matches_from(cmd);
        assert!(
            get_config_paths_from_cli(matches).is_err(),
            "No configuration file specified"
        )
    }
    #[test]
    fn no_default_config_file() {
        rename("example_config.toml", "tmp_test.toml").unwrap();
        assert!(get_config_path_from_default().is_err());
        rename("tmp_test.toml", "example_config.toml").unwrap();
    }
}
