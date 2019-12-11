#[macro_use]
extern crate futures;
#[macro_use]
extern crate log;
#[macro_use]
extern crate thiserror;

mod uris;

pub use self::uris::*;

use async_fetcher::{Fetcher, Source};
use futures::{channel::mpsc, prelude::*};
use std::{path::Path, sync::Arc};
use surf::middleware::HttpClient;

const ARCHIVES: &str = "/var/cache/apt/archives/";
const PARTIAL: &str = "/var/cache/apt/archives/partial/";

pub async fn fetch_updates<C: HttpClient>(fetcher: Arc<Fetcher<C>>) -> anyhow::Result<()> {
    // Create an async rendezvouz channel for deb URI transmissions.
    let (tx, rx) = mpsc::channel(0);

    // Spawns an apt-get child process to parse a list of URIs to be fetched.
    let uri_fetcher = crate::update_uris(tx);

    // Receives the URIs as they become available, submitting them to the fetcher.
    let package_fetcher = async move {
        let sources = rx.map(|package| Source {
            urls: Arc::from(vec![package.uri]),
            dest: Arc::from(Path::new(ARCHIVES).join(&*package.name)),
            part: Some(Arc::from(Path::new(PARTIAL).join(&*package.name))),
        });

        fetcher.from_stream(sources).await
    }
    .boxed_local();

    // Concurrently await on each of the interdependent futures.
    let (res, _) = futures::future::join(uri_fetcher, package_fetcher).await;

    res
}
