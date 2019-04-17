use apt_sources_lists::*;
use dist::{REQUIRED_DIST_FILES, update::{Updater, DistUpdateError}};
use futures::{self, Future, future::lazy};
use keyring::AptKeyring;
use reqwest::{self, async::Client};
use std::io;
use tokio::{self, prelude::future::*};
use std::sync::Arc;

use tokio::runtime::Runtime;

#[derive(Debug, Error)]
pub enum DistUpgradeError {
    #[error(display = "tokio error: failure {}: {}", what, why)]
    Tokio { what: &'static str, why: tokio::io::Error },
    #[error(display = "some apt sources could not be reached")]
    SourcesUnavailable { success: Vec<String>, failure: Vec<(String, reqwest::Error)> },
    #[error(display = "http/s request failed: {}", why)]
    Request { why: reqwest::Error },
    #[error(display = "failed to overwrite apt source files: {}", why)]
    AptFileOverwrite { why: io::Error },
    #[error(display = "failed to fetch apt sources: {}", why)]
    AptFileUpdate { why: DistUpdateError },
    #[error(display = "failed to fetch apt sources[0] and restore apt files[1]: \n\t[0] {}\n\t[1] {}", why, file_why)]
    AptFileUpdateRestore { why: DistUpdateError, file_why: Box<DistUpgradeError> },
}

/// Build an upgrade request, and check if the upgrade is possible.
pub struct UpgradeRequest<'a> {
    client: Arc<Client>,
    list: SourcesList,
    keyring: Option<Arc<AptKeyring>>,
    runtime: &'a mut Runtime
}

impl<'a> UpgradeRequest<'a> {
    /// Constructs a new upgrade request from a given async client and apt sources list.
    pub fn new(client: Arc<Client>, list: SourcesList, runtime: &'a mut Runtime) -> Self {
        Self { client, keyring: None, list, runtime }
    }

    pub fn keyring(mut self, keyring: Arc<AptKeyring>) -> Self {
        self.keyring = Some(keyring);
        self
    }

    /// Check if the upgrade request is possible, and enable upgrading if so.
    pub fn send<S: Into<Arc<str>>>(self, from_suite: S, to_suite: S) -> Result<Upgrader, DistUpgradeError> {
        let from_suite = from_suite.into();
        let to_suite = to_suite.into();

        let results = {
            let requests = head_all_release_files(self.client.clone(), &self.list, &from_suite, &to_suite);
            self.runtime.block_on(futures::future::join_all(requests))
                .expect("errors not permitted to be returned")
        };

        if results.iter().any(|e| e.1.is_err()) {
            let (success, failure): (Vec<String>, Vec<(String, reqwest::Error)>) = results.into_iter()
                .fold((Vec::new(), Vec::new()), |(mut succ, mut fail), (entry, result)| {
                    // If not already a recorded failure
                    if !fail.iter().any(|&(ref url, _)| url == &entry.url) {
                        let succ_pos = succ.iter().position(|e| e == &entry.url);

                        match result {
                            // Add to success if a prior success is not recorded.
                            Ok(()) => if succ_pos.is_none() {
                                succ.push(entry.url);
                            }
                            // Add to failure if it's an error.
                            Err(why) => {
                                // Remove it from the success vec if it was previously successful.
                                if let Some(pos) = succ_pos {
                                    succ.swap_remove(pos);
                                }

                                fail.push((entry.url, why));
                            }
                        }
                    }

                    (succ, fail)
                });

            return Err(DistUpgradeError::SourcesUnavailable { success, failure });
        }

        Ok(Upgrader {
            client: self.client,
            keyring: self.keyring,
            list: self.list,
            from_suite,
            to_suite
        })
    }
}

/// An upgrader is created from an `UpgradeRequest::send`, which ensures that the dist upgrade is possible.
pub struct Upgrader {
    client: Arc<Client>,
    keyring: Option<Arc<AptKeyring>>,
    list: SourcesList,
    from_suite: Arc<str>,
    to_suite: Arc<str>,
}

impl Upgrader {
    /// Modify the apt sources in the system, and fetch the new dist files.
    ///
    /// On failure, the upgrader is returned alongside an error indicating the cause.
    /// On success, this upgrader is consumed, as it is no longer valid.
    pub fn dist_upgrade(mut self) -> Result<Vec<(String, Result<(), DistUpdateError>)>, (Self, DistUpgradeError)> {
        self.overwrite_apt_sources()
            .and_then(|_| self.update_dist_files())
            .map_err(|why| (self, why))
    }

    /// Attempt to overwrite the apt sources with the new suite to upgrade to.
    pub fn overwrite_apt_sources(&mut self) -> Result<(), DistUpgradeError> {
        self.list.dist_upgrade(&self.from_suite, &self.to_suite)
            .map_err(|why| DistUpgradeError::AptFileOverwrite { why })
    }

    /// Performs the reverse of the overwrite method.
    pub fn revert_apt_sources(&mut self) -> Result<(), DistUpgradeError> {
        self.list.dist_upgrade(&self.to_suite, &self.from_suite)
            .map_err(|why| DistUpgradeError::AptFileOverwrite { why })
    }

    /// Attempt to fetch new dist files from the new sources.
    fn update_dist_files(&mut self) -> Result<Vec<(String, Result<(), DistUpdateError>)>, DistUpgradeError> {
        let result = {
            let client = self.client.clone();

            let mut updater = Updater::new(client, &self.list);

            if let Some(ref keyring) = self.keyring {
                updater = updater.keyring(keyring.clone());
            }

            updater.tokio_update()
        };

        result.map_err(|why| {
            match self.overwrite_apt_sources().map_err(Box::new) {
                Ok(_) => DistUpgradeError::AptFileUpdate { why },
                Err(file_why) => DistUpgradeError::AptFileUpdateRestore { why, file_why }
            }
        })
    }
}

/// Construct an iterator of futures for fetching each dist release file of each source.
fn head_all_release_files<'a>(
    client: Arc<Client>,
    list: &SourcesList,
    from_suite: &str,
    to_suite: &str,
) -> impl Iterator<Item = impl Future<Item = (SourceEntry, Result<(), reqwest::Error>), Error = ()>> {
    let urls = list.dist_paths()
        .filter(|entry| entry.url.starts_with("http") && entry.suite.starts_with(from_suite))
        .map(|file| {
            let mut file = file.clone();
            file.suite = file.suite.replace(from_suite, to_suite);
            file
        })
        .collect::<Vec<SourceEntry>>();

    urls.into_iter()
        .map(move |mut file| {
            head_release_files(client.clone(), file)
        })
}

/// Construct an iterator of futures for fetching each dist release file.
fn head_release_files(
    client: Arc<Client>,
    entry: SourceEntry,
) -> impl Future<Item = (SourceEntry, Result<(), reqwest::Error>), Error = ()> {
    let mut url = entry.dist_path();
    if ! url.ends_with('/') {
        url.push('/')
    }

    let futures = REQUIRED_DIST_FILES.iter().map(move |file| {
        let file = [url.as_str(), file].concat();
        let client = client.clone();

        lazy(move || {
            println!("HEAD {}", file);
            client.head(&file)
                .send()
                .and_then(|response| response.error_for_status())
                .then(move |v| {
                    println!("HIT {}", file);
                    v
                })
        })
    });

    join_all(futures).then(move |results| lazy(move || {
        Ok((entry, results.map(|_| ())))
    }))
}
