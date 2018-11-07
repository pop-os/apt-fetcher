extern crate apt_sources_lists;
extern crate failure;
#[macro_use]
extern crate failure_derive;
extern crate futures;
extern crate reqwest;
extern crate tokio;

use apt_sources_lists::*;
use futures::Future;
use reqwest::async::{Client, Response};
use tokio::runtime::current_thread::Runtime;

/// Release files which must exist within an apt repository's dists directory.
pub const RELEASE_FILES: &[&str] = &["InRelease","Release","Release.gpg"];

#[derive(Debug, Fail)]
pub enum DistUpgradeError {
    #[fail(display = "tokio error: failure {}: {}", what, why)]
    Tokio { what: &'static str, why: tokio::io::Error },
    #[fail(display = "failed to get apt sources: {}", why)]
    SourceList { why: SourceError },
    #[fail(display = "http/s request failed: {}", why)]
    Request { why: reqwest::Error }
}

pub fn dist_upgrade_possible(from_suite: &str, to_suite: &str) -> Result<(), DistUpgradeError> {
    let list = SourcesList::scan().map_err(|why| DistUpgradeError::SourceList { why })?;
    let client = Client::new();

    let requests = fetch_all_dist_release_files(&client, &list, from_suite, to_suite);
    let future = futures::future::join_all(requests);

    let mut runtime = Runtime::new()
        .map_err(|why| {
            DistUpgradeError::Tokio { what: "constructing single-threaded runtime", why }
        })?;

    runtime.block_on(future)
        .map(|_| ())
        .map_err(|why| DistUpgradeError::Request { why })
}

/// Construct an iterator of futures for fetching each dist release file of each source.
fn fetch_all_dist_release_files<'a>(
    client: &'a Client,
    list: &'a SourcesList,
    from_suite: &'a str,
    to_suite: &'a str,
) -> impl Iterator<Item = impl Future<Item = Response, Error = reqwest::Error>> + 'a {
    list.dist_upgrade_paths(from_suite, to_suite)
        .flat_map(move |url| fetch_dist_release_files(client, url))
}

/// Construct an iterator of futures for fetching each dist release file.
fn fetch_dist_release_files<'a>(
    client: &'a Client,
    url: String
) -> impl Iterator<Item = impl Future<Item = Response, Error = reqwest::Error>>  + 'a{
    let requires_slash = url.ends_with('/');

    RELEASE_FILES.into_iter().map(move |file| {
        let url = if requires_slash {
            [url.as_str(), file].concat()
        } else {
            [url.as_str(), "/", file].concat()
        };

        client.head(&url)
            .send()
            .and_then(|response| response.error_for_status())
    })
}
