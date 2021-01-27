## Alis unofficial IRC bot
[![build](https://github.com/precambrien/alis-bot-rs/workflows/build/badge.svg)](https://github.com/precambrien/alis-bot-rs/actions?query=branch%3Amaster+event%3Apush+workflow%3Abuild)
[![test](https://github.com/precambrien/alis-bot-rs/workflows/tests/badge.svg)](https://github.com/precambrien/alis-bot-rs/actions?query=branch%3Amaster+event%3Apush+workflow%3Atests)

IRC bot allowing to search for channels with more flexibility than the /LIST command.
This bot is inspired by alis's service on Freenode. 

Bot usage:

    <user> /msg alis-bot-rs list *foo* --topic ?bar --min 10 --max 50
Shows a list of channels whose name matches *\*foo\** and topic matches *?bar*, with at least 10 users and at most 50 users.
For full command syntax and options, ask *alis-bot-rs* directly  : 

	<user> /msg alis-bot-rs help

## Build

### Get Rust

[https://www.rust-lang.org/en-US/install.html](https://www.rust-lang.org/en-US/install.html)

### Build

    git clone https://github.com/precambrien/alis-bot-rs
    cd alis-bot-rs
    cargo build
    
### Run tests

    cargo test
    
## Configuration

A configuration file is required to specify IRC server details, such as address of the IRC server, bot nickname and connection credentials. 
This config file can be specified with one of this argument :
`--conf=<file>` 
`--conf=<file1><file2>` : allows alis-bot-rs to connect to multiple servers.
`--conf-dir=<directory>` : search for all *.toml file in directory (non-recursive). Files missing the `server` option will be considered unvalid.
If no configuration file is provided, alis-bot-rs will use the default configuration file `example_configuration.toml` in this crate directory.

### Example

    alis-bot-rs -c freenode_config.toml geeknode_config.toml

Or for full usage and options:

    alis-bot-rs -h
