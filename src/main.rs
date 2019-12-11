use apt_fetcher_preview::fetch_updates;
use async_fetcher::{FetchEvent, Fetcher};
use futures::{channel::mpsc, prelude::*};
use std::{error::Error as _, sync::Arc, time::Duration};

fn main() {
    futures::executor::block_on(async move {
        let (etx, mut erx) = mpsc::unbounded();

        // Handles events from the package fetcher
        let event_handler = async move {
            while let Some(event) = erx.next().await {
                match event {
                    FetchEvent::ContentLength(dest, total) => {
                        println!("{}: total {}", dest.display(), total);
                    }
                    FetchEvent::Fetched(dest, result) => match result {
                        Ok(()) => println!("fetched {}", dest.display()),
                        Err(why) => {
                            eprintln!("fetching {} failed: {}", dest.display(), why);
                            let mut source = why.source();
                            while let Some(why) = source {
                                eprintln!("    caused by: {}", why);
                                source = why.source();
                            }
                        }
                    },
                    FetchEvent::Fetching(dest) => {
                        println!("fetching {}", dest.display());
                    }
                    FetchEvent::Progress(_dest, _written) => {}
                    FetchEvent::PartFetching(dest, part) => {
                        println!("fetching part {} of {}", part, dest.display());
                    }
                    FetchEvent::PartFetched(dest, part) => {
                        println!("fetched part {} of {}", part, dest.display());
                    }
                }
            }
        };

        let fetcher = Arc::new(
            Fetcher::default()
                .concurrent_files(4)
                .connections_per_file(4)
                .part_size(4 * 1024 * 1024)
                .timeout(Duration::from_secs(30))
                .events(etx),
        );

        let (res, _) = futures::join!(fetch_updates(fetcher), event_handler);

        if let Err(why) = res {
            eprintln!("error: {}", why);
        }
    });
}
