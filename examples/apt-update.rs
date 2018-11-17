extern crate apt_fetcher;
extern crate apt_keyring;
#[macro_use]
extern crate clap;
extern crate reqwest;
extern crate log;

use apt_fetcher::*;
use apt_keyring::AptKeyring;
use reqwest::async::Client;
use std::process::exit;
use log::{Record, Level, Metadata};
use std::sync::Arc;

struct SimpleLogger;

impl log::Log for SimpleLogger {
    fn enabled(&self, metadata: &Metadata) -> bool {
        metadata.level() <= Level::Trace
    }

    fn log(&self, record: &Record) {
        if self.enabled(record.metadata()) {
            println!("{} - {}", record.level(), record.args());
        }
    }

    fn flush(&self) {}
}

use log::{SetLoggerError, LevelFilter};

static LOGGER: SimpleLogger = SimpleLogger;

pub fn init() -> Result<(), SetLoggerError> {
    // log::set_logger(&LOGGER)
    //     .map(|()| log::set_max_level(LevelFilter::Trace))?;

    Ok(())
}

pub fn main() {
    let sources = SourcesList::scan().unwrap();
    let client = Arc::new(Client::new());
    let keyring = Arc::new(AptKeyring::new().unwrap());

    let result = Updater::new(client, &sources)
        .keyring(keyring)
        .tokio_update();

    println!("result: {:?}", result);
}
