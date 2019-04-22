extern crate apt_fetcher;
extern crate apt_keyring;
extern crate clap;
extern crate futures;
extern crate log;
extern crate reqwest;
extern crate tokio;
extern crate tokio_threadpool;

use apt_fetcher::*;
use apt_keyring::AptKeyring;
use futures::{lazy, Future};
use reqwest::async::Client;
use std::sync::Arc;
use std::time::Instant;
use tokio_threadpool::ThreadPool;

pub fn main() {
    init_logging().unwrap();

    let start = Instant::now();
    let sources = SourcesLists::scan().unwrap();
    let client = Arc::new(Client::new());
    let keyring = Arc::new(AptKeyring::new().unwrap());

    // A futures runtime is required to execute the updater.
    let pool = ThreadPool::new();

    // The updater must by executed on the runtime -- achieved via a lazy future.
    let updater = lazy(move || {
        Updater::new(client, &sources)
            .keyring(keyring)
            .tokio_update()
    });

    let result = pool.spawn_handle(updater).wait();

    println!("update finished in {:?}", Instant::now() - start);

    match result {
        Ok(_) => println!("success"),
        Err(why) => eprintln!("failed to update: {:?}", why)
    }
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
            && record.target().starts_with("apt_fetcher")
        {
            eprintln!("{}", record.args());
        }
    }

    fn flush(&self) {}
}
