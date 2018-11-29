use async_fetcher::{AsyncFetcher, FetchError};
use deb_architectures::{Architecture, supported_architectures};
use deb_release_file::{DistRelease, ReleaseEntry, ReleaseVariant};
use apt_sources_lists::*;
use decompressor::{Compression, Decompressor};
use keyring::AptKeyring;
use filetime;
use futures::{self, Future, Stream};
use gpgrv::{Keyring, verify_clearsign_armour};
use reqwest::{self, async::{Client, Decoder, Response}};
use std::io::{self, BufRead, BufReader, Error as IoError, Seek, SeekFrom, Write};
use std::process::Command;
use tempfile_fast::PersistableTempFile;
use tokio::runtime::Runtime;
use tokio::{self, fs::File, io::Read};
use status::StatusExt;
use std::{fs::File as SyncFile, sync::Arc, path::{Path, PathBuf}};

pub type Url = Arc<String>;
pub type Data = Arc<String>;
pub type ReleasePath = String;

const LISTS: &str = "/var/lib/apt/lists/";
const PARTIAL: &str = "/var/lib/apt/lists/partial/";
const CHECKSUM_PRIORITY: &[&str] = &["SHA256", "SHA1", "MD5Sum"];

#[derive(Debug, Fail)]
pub enum DistUpdateError {
    #[fail(display = "tokio error: failure {}: {}", what, why)]
    Tokio { what: &'static str, why: tokio::io::Error },
    #[fail(display = "http/s request failed: {}", why)]
    Request { why: reqwest::Error },
    #[fail(display = "failed to exec 'apt update': {}", why)]
    AptUpdate { why: io::Error },
    #[fail(display = "decoder error: {}", why)]
    Decoder { why: io::Error },
    #[fail(display = "failed to validate GPG signature: {}", why)]
    GpgValidation { why: ::failure::Error },
    #[fail(display = "failed to read local apt repo: {}", why)]
    LocalRepo { why: IoError },
    #[fail(display = "release file had invalid UTF-8")]
    InvalidUtf8,
    #[fail(display = "invalid release data: {}", why)]
    InvalidReleaseData { why: io::Error },
    #[fail(display = "no entries were found for the repository at {}", repo)]
    NoEntriesFound { repo: Url },
    #[fail(display = "failed to fetch available architectures: {}", why)]
    Architectures { why: io::Error },
    #[fail(display = "failed to initialize decompressor: {}", why)]
    Decompressor { why: io::Error },
    #[fail(display = "failed to fetch file: {}", why)]
    Fetcher { why: FetchError },
}

pub struct Updater<'a> {
    client: Arc<Client>,
    sources_list: &'a SourcesList,
    keyring: Option<Arc<AptKeyring>>,
}

impl<'a> Updater<'a> {
    pub fn new(client: Arc<Client>, sources_list: &'a SourcesList) -> Self {
        Self { client, keyring: None, sources_list }
    }

    pub fn keyring(mut self, keyring: Arc<AptKeyring>) -> Self {
        self.keyring = Some(keyring);
        self
    }

    pub fn update(&self) -> Result<(), DistUpdateError> {
        Command::new("apt")
            .args(&["update"])
            .status()
            .and_then(StatusExt::as_result)
            .map_err(|why| DistUpdateError::AptUpdate { why })
    }

    /// Experimental apt update replacement.
    pub fn tokio_update(&self) -> Result<(), DistUpdateError> {
        let architectures = supported_architectures()
            .map(Arc::new)
            .map_err(|why| DistUpdateError::Architectures { why })?;

        let mut runtime = Runtime::new()
            .map_err(|why| {
                DistUpdateError::Tokio { what: "constructing runtime", why }
            })?;

        // First, fetch all the release files and parse them.
        let mut requests = ReleaseFetcher::new(self.client.clone());

        // Use a keyring if it was supplied.
        if let Some(keyring) = self.keyring.as_ref() {
            requests = requests.with_keyring(keyring.clone());
        }

        let release_files = futures::future::join_all(requests.fetch_releases(&self.sources_list));
        let release_file_results = Arc::new(runtime.block_on(release_files)?);

        eprintln!("results: {:?}", release_file_results);

        // // Then, fetch all the files mentioned in those release files.
        // // TODO: Write the temporary files to their permanent locations and set the file times.
        // let futures = fetch_all_files(&self.client, &release_file_results, &architectures)?;
        // let results = runtime.block_on(futures::future::join_all(futures))?;

        Ok(())
    }
}

pub struct ReleaseFetcher {
    client: Arc<Client>,
    keyring: Option<Arc<AptKeyring>>
}

impl ReleaseFetcher {
    pub fn new(client: Arc<Client>) -> Self {
        Self { client, keyring: None }
    }

    pub fn with_keyring(mut self, keyring: Arc<AptKeyring>) -> Self {
        self.keyring = Some(keyring);
        self
    }

    pub fn fetch_releases(self, list: &SourcesList)
        -> impl Iterator<Item = impl Future<Item = (), Error = DistUpdateError>>
    {
        let to_fetch = list.dist_paths()
            // Fetch the information we need to create our requests for the release files.
            .map(move |source_entry| {
                ReleaseFetch {
                    trusted: source_entry.options.iter().any(|x| x.as_str() == "trusted=yes"),
                    dist_path: source_entry.dist_path(),
                    components: source_entry.components.clone()
                }
            })
            .collect::<Vec<ReleaseFetch>>();

        let keyring = self.keyring;
        let client = self.client;

        // Fetch them if we need to, then parse their data into memory.
        to_fetch.into_iter().map(move |request| {
            let inrelease = [&request.dist_path, "/InRelease"].concat();
            let dest_file_name = inrelease[7..].replace("/", "_");

            let dest: PathBuf = [LISTS, &dest_file_name].concat().into();
            let partial: PathBuf = [PARTIAL, &dest_file_name].concat().into();

            // TODO:
            // - Handle local repos with the file:// url scheme
            // - Handle trusted repos (fetch the release file instead)

            let future = {
                let dest = dest.clone();
                AsyncFetcher::new(&client, inrelease)
                    .request_to_path(dest.clone())
                    .then_download(partial.clone())
                    .then_rename()
                    .into_future()
                    .map_err(|why| DistUpdateError::Fetcher { why })
            };

            let future = future.map(|_| ReleaseData {
                trusted: request.trusted,
                path: dest,
                components: request.components
            });

            let future = ReleaseFetched::new(future, keyring.clone())
                .validate_releases()
                .into_future();

            future.map(|_| ())
        })
    }
}

pub struct ReleaseFetched<T: Future<Item = ReleaseData, Error = DistUpdateError> + Send> {
    future: T,
    keyring: Option<Arc<AptKeyring>>
}


impl<T: Future<Item = ReleaseData, Error = DistUpdateError> + Send> ReleaseFetched<T> {
    pub fn new(future: T, keyring: Option<Arc<AptKeyring>>) -> Self {
        Self { future, keyring }
    }

    pub fn validate_releases(self) -> ValidatedRelease<impl Future<Item = ReleaseInfo, Error = DistUpdateError> + Send> {
        let future = self.future;
        let keyring = self.keyring;

        ValidatedRelease {
            future: future.and_then(|release_data| {
                futures::future::lazy(move || {
                    let mut release_file = SyncFile::open(&release_data.path)
                        .expect("release file not found");

                    let mut output = Vec::new();

                    if release_data.trusted {
                        unimplemented!()
                    } else {
                        debug!("verifying GPG signature of {}", release_data.path.display());
                        verify_clearsign_armour(&mut BufReader::new(release_file), &mut output, &keyring.expect("keyring required"))
                            .map_err(|why| DistUpdateError::GpgValidation { why })?;
                    }


                    debug!("parsing release file at {}", release_data.path.display());
                    let release: DistRelease = String::from_utf8(output)
                        .map_err(|_| DistUpdateError::InvalidUtf8)
                        .and_then(|string| {
                            string.parse::<DistRelease>()
                                .map_err(|why| DistUpdateError::InvalidReleaseData { why })
                        })?;

                    Ok(ReleaseInfo { release, components: release_data.components })
                })
            })
        }
    }
}

pub struct ValidatedRelease<T: Future<Item = ReleaseInfo, Error = DistUpdateError> + Send> {
    future: T,
}

impl<T: Future<Item = ReleaseInfo, Error = DistUpdateError> + Send> ValidatedRelease<T> {
    pub fn into_future(self) -> T { self.future }
}

pub struct ReleaseFetch {
    /// If we trust the repo, we will not require a keyring.
    trusted: bool,
    /// The base directory of this suites dist path.
    dist_path: String,
    /// We should only fetch files from these components in the release file.
    components: Vec<String>
}

pub struct ReleaseData {
    trusted: bool,
    path: PathBuf,
    components: Vec<String>
}

pub struct ReleaseInfo {
    release: DistRelease,
    components: Vec<String>
}
