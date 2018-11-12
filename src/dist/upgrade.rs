use apt_sources_lists::*;
use dist::{REQUIRED_DIST_FILES, update::{Updater, DistUpdateError}};
use futures::Future;
use reqwest::async::{Client, Response};
use std::io;
use tokio::runtime::current_thread::Runtime;

#[derive(Debug, Fail)]
pub enum DistUpgradeError {
    #[fail(display = "tokio error: failure {}: {}", what, why)]
    Tokio { what: &'static str, why: tokio::io::Error },
    #[fail(display = "http/s request failed: {}", why)]
    Request { why: reqwest::Error },
    #[fail(display = "failed to overwrite apt source files: {}", why)]
    AptFileOverwrite { why: io::Error },
    #[fail(display = "failed to fetch apt sources: {}", why)]
    AptFileUpdate { why: DistUpdateError },
    #[fail(display = "failed to fetch apt sources[0] and restore apt files[1]: \n\t[0] {}\n\t[1] {}", why, file_why)]
    AptFileUpdateRestore { why: DistUpdateError, file_why: String },
}

/// Build an upgrade request, and check if the upgrade is possible.
pub struct UpgradeRequest<'a> {
    client: &'a Client,
    list: &'a mut SourcesList
}

impl<'a> UpgradeRequest<'a> {
    /// Constructs a new upgrade request from a given async client and apt sources list.
    pub fn new(client: &'a Client, list: &'a mut SourcesList) -> Self {
        Self { client, list }
    }

    /// Check if the upgrade request is possible, and enable upgrading if so.
    pub fn send(self, from_suite: &'a str, to_suite: &'a str) -> Result<Upgrader<'a>, DistUpgradeError> {
        let result = {
            let requests = fetch_all_dist_release_files(&self.client, &self.list, from_suite, to_suite);
            let future = futures::future::join_all(requests);

            let mut runtime = Runtime::new()
                .map_err(|why| {
                    DistUpgradeError::Tokio { what: "constructing single-threaded runtime", why }
                })?;

            runtime.block_on(future)
                .map(|_| ())
                .map_err(|why| DistUpgradeError::Request { why })
        };

        result.map(move |_| Upgrader { client: self.client, list: self.list, from_suite, to_suite })
    }
}

/// An upgrader is created from an `UpgradeRequest::send`, which ensures that the dist upgrade is possible.
pub struct Upgrader<'a> {
    client: &'a Client,
    list: &'a mut SourcesList,
    from_suite: &'a str,
    to_suite: &'a str
}

impl<'a> Upgrader<'a> {
    /// Modify the apt sources in the system, and fetch the new dist files.
    ///
    /// On failure, the upgrader is returned alongside an error indicating the cause.
    /// On success, this upgrader is consumed, as it is no longer valid.
    pub fn dist_upgrade(mut self) -> Result<(), (Self, DistUpgradeError)> {
        match self.overwrite_apt_sources().and_then(|_| self.update_dist_files()) {
            Ok(()) => Ok(()),
            Err(why) => Err((self, why))
        }
    }

    /// Attempt to overwrite the apt sources with the new suite to upgrade to.
    fn overwrite_apt_sources(&mut self) -> Result<(), DistUpgradeError> {
        self.list.dist_upgrade(self.from_suite, self.to_suite)
            .map_err(|why| DistUpgradeError::AptFileOverwrite { why })
    }

    /// Attempt to fetch new dist files from the new sources.
    fn update_dist_files(&mut self) -> Result<(), DistUpgradeError> {
        Updater::new(&self.client, &self.list)
            .update()
            .map_err(|why| {
                match self.overwrite_apt_sources().map_err(|why| format!("{}", why)) {
                    Ok(_) => DistUpgradeError::AptFileUpdate { why },
                    Err(file_why) => DistUpgradeError::AptFileUpdateRestore { why, file_why }
                }
            })
    }
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

    REQUIRED_DIST_FILES.iter().map(move |file| {
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
