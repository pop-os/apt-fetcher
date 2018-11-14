extern crate apt_sources_repos;
#[macro_use]
extern crate clap;
extern crate reqwest;
extern crate log;

use log::{Record, Level, Metadata};

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

use apt_sources_repos::*;
use reqwest::async::Client;
use std::process::exit;
use std::sync::{Arc, Mutex};

pub fn main() {
    init().unwrap();
    let matches = clap_app! {
        release_upgrade =>
            (@arg from: +required "upgrade from this suite")
            (@arg to: +required "to this suite")
            (@arg core: --core +multiple +takes_value "ensuring that these packages are kept")
    }.get_matches();

    let from = matches.value_of("from").unwrap();
    let to = matches.value_of("to").unwrap();
    let core_packages = matches.values_of("core").unwrap();

    let sources = Arc::new(Mutex::new(SourcesList::scan().unwrap()));
    let client = Arc::new(Client::new());
    let keyring = Arc::new(AptKeyring::new().unwrap());

    let request = UpgradeRequest::new(client, sources)
        .keyring(keyring)
        .send(from, to);

    match request {
        Ok(upgrader) => {
            println!("Upgrading from {} to {}", from, to);

            if let Err((_, why)) = upgrader.dist_upgrade() {
                eprintln!("failed to upgrade dist files: {}", why);
                exit(1);
            }

            if let Err(why) = upgrade(core_packages) {
                eprintln!("failed to upgrade packages: {}", why);
                exit(1);
            }

            println!("Upgrade was successful");
        },
        Err(why) => {
            eprintln!("upgrade not possible: {}", why);
            exit(1);
        }
    }
}
