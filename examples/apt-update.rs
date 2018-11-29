extern crate apt_fetcher;
extern crate apt_keyring;
#[macro_use]
extern crate clap;
extern crate reqwest;
extern crate log;

use apt_fetcher::*;
use apt_keyring::AptKeyring;
use reqwest::async::Client;
use std::sync::Arc;
use std::time::Instant;

pub fn main() {
    init_logging().unwrap();

    let start = Instant::now();
    let sources = SourcesList::scan().unwrap();
    let client = Arc::new(Client::new());
    let keyring = Arc::new(AptKeyring::new().unwrap());

    let result = Updater::new(client, &sources)
        .keyring(keyring)
        .tokio_update();

    println!("update finished in {:?}", Instant::now() - start);
    println!("fetched {:#?}", result);
}


// Configuring the logger

use log::{Level, LevelFilter, Metadata, Record, SetLoggerError};

static LOGGER: SimpleLogger = SimpleLogger;

pub fn init_logging() -> Result<(), SetLoggerError> {
    log::set_logger(&LOGGER).map(|()| log::set_max_level(LevelFilter::Debug))
}

struct SimpleLogger;

impl log::Log for SimpleLogger {
    fn enabled(&self, metadata: &Metadata) -> bool {
        metadata.level() <= Level::Debug
    }

    fn log(&self, record: &Record) {
        if self.enabled(record.metadata())
            && (record.target() == "async_fetcher"
            || record.target() == "apt_fetcher")
        {
            eprintln!("{}: {} - {}", record.target(), record.level(), record.args());
        }
    }

    fn flush(&self) {}
}
