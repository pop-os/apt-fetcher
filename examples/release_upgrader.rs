extern crate apt_sources_repos;
#[macro_use]
extern crate clap;
extern crate reqwest;

use apt_sources_repos::*;
use reqwest::async::Client;
use std::process::exit;

pub fn main() {
    let matches = clap_app! {
        release_upgrade =>
            (@arg from: +required "upgrade from this suite")
            (@arg to: +required "to this suite")
            (@arg core: --core +multiple +takes_value "ensuring that these packages are kept")
    }.get_matches();

    let from = matches.value_of("from").unwrap();
    let to = matches.value_of("to").unwrap();
    let core_packages = matches.values_of("core").unwrap();

    let mut sources = SourcesList::scan().unwrap();
    let client = Client::new();

    match UpgradeRequest::new(&client, &mut sources).send(from, to) {
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
