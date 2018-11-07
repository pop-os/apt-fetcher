extern crate apt_sources_repos;

use apt_sources_repos::*;

pub fn main() {
    match dist_upgrade_possible("cosmic", "cosmic") {
        Ok(()) => println!("upgrade possible"),
        Err(why) => eprintln!("upgrade not possible: {}", why)
    }
}
