use apt_sources_lists::*;
use dist::{REQUIRED_DIST_FILES, update::{Updater, DistUpdateError}};
use futures::{self, Future, future::lazy};
use keyring::AptKeyring;
use reqwest::{self, async::{Client, Response}};
use std::io;
use tokio;
use std::sync::{Arc, Mutex};

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
    AptFileUpdateRestore { why: DistUpdateError, file_why: Box<DistUpgradeError> },
}

/// Build an upgrade request, and check if the upgrade is possible.
pub struct UpgradeRequest {
    client: Arc<Client>,
    list: Arc<Mutex<SourcesList>>,
    keyring: Option<Arc<AptKeyring>>,
}

impl UpgradeRequest {
    /// Constructs a new upgrade request from a given async client and apt sources list.
    pub fn new(client: Arc<Client>, list: Arc<Mutex<SourcesList>>) -> Self {
        Self { client, keyring: None, list }
    }

    pub fn keyring(mut self, keyring: Arc<AptKeyring>) -> Self {
        self.keyring = Some(keyring);
        self
    }

    /// Check if the upgrade request is possible, and enable upgrading if so.
    pub fn send<S: Into<Arc<str>>>(self, from_suite: S, to_suite: S) -> Result<Upgrader, DistUpgradeError> {
        let from_suite = from_suite.into();
        let to_suite = to_suite.into();

        let result = {
            let requests = head_all_release_files(self.client.clone(), &self.list, &from_suite, &to_suite);
            use tokio_threadpool::ThreadPool;

            let pool = ThreadPool::new();
            let handle = pool.spawn_handle(
                futures::future::join_all(requests)
            );

            handle.wait()
                .map_err(|why| DistUpgradeError::Request { why })
        };

        result.map(move |_| Upgrader {
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
    list: Arc<Mutex<SourcesList>>,
    from_suite: Arc<str>,
    to_suite: Arc<str>
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
        self.list.lock().unwrap().dist_upgrade(&self.from_suite, &self.to_suite)
            .map_err(|why| DistUpgradeError::AptFileOverwrite { why })
    }

    /// Attempt to fetch new dist files from the new sources.
    fn update_dist_files(&mut self) -> Result<Vec<(String, Result<(), DistUpdateError>)>, DistUpgradeError> {
        let result = {
            let client = self.client.clone();
            let list = self.list.lock().unwrap();

            let mut updater = Updater::new(client, &list);

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
fn head_all_release_files(
    client: Arc<Client>,
    list: &Arc<Mutex<SourcesList>>,
    from_suite: &str,
    to_suite: &str,
) -> impl Iterator<Item = impl Future<Item = Response, Error = reqwest::Error>> {
    let urls = {
        let lock = list.lock().unwrap();
        let vector = lock.dist_upgrade_paths(from_suite, to_suite).collect::<Vec<String>>();
        drop(lock);
        vector
    };

    urls.into_iter()
        .flat_map(move |url| head_release_files(client.clone(), url))
}

/// Construct an iterator of futures for fetching each dist release file.
fn head_release_files(
    client: Arc<Client>,
    mut url: String
) -> impl Iterator<Item = impl Future<Item = Response, Error = reqwest::Error>> {
    if ! url.ends_with('/') {
        url.push('/')
    }

    REQUIRED_DIST_FILES.iter().map(move |file| {
        let url = [url.as_str(), file].concat();
        let client = client.clone();

        lazy(move || {
            println!("HEAD {}", url);
            client.head(&url)
                .send()
                .and_then(|response| response.error_for_status())
                .then(move |v| {
                    println!("HIT {}", url);
                    v
                })
        })
    })
}
