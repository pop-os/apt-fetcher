use async_fetcher::{AsyncFetcher, CompletedState, FetchError};
use deb_architectures::{Architecture, supported_architectures};
use apt_release_file::{DistRelease, ReleaseEntry, ReleaseVariant};
use apt_sources_lists::*;
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
use std::{fs::{self as sync_fs, File as SyncFile}, sync::Arc, path::{Path, PathBuf}};
use flate2::write::GzDecoder;
use xz2::write::XzDecoder;
use md5::Md5;
use sha1::Sha1;
use sha2::Sha256;

pub type Url = Arc<str>;
pub type ReleasePath = String;

#[derive(Copy, Clone, Debug, PartialEq)]
pub enum ChecksumKind {
    Sha256,
    Sha1,
    Md5
}

const LISTS: &str = "var/lib/apt/lists/";
const PARTIAL: &str = "var/lib/apt/lists/partial/";

const CHECKSUM_PRIORITY: &[(&str, ChecksumKind)] = &[
    ("SHA256", ChecksumKind::Sha256),
    ("SHA1", ChecksumKind::Sha1),
    ("MD5Sum", ChecksumKind::Md5)
];

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

        if ! Path::new(PARTIAL).exists() {
            let _ = sync_fs::create_dir_all(PARTIAL);
        }

        // First, fetch all the release files and parse them.
        let mut requests = ReleaseFetcher::new(self.client.clone());

        // Use a keyring if it was supplied.
        if let Some(keyring) = self.keyring.as_ref() {
            requests = requests.with_keyring(keyring.clone());
        }

        let release_files = futures::future::join_all(
            requests.fetch_updates(&self.sources_list, architectures)
        );

        let release_file_results = runtime.block_on(release_files)?;

        eprintln!("results: {:?}", release_file_results);

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

    pub fn fetch_updates(self, list: &SourcesList, archs: Arc<Vec<Architecture>>)
        -> impl Iterator<Item = impl Future<Item = (), Error = DistUpdateError>>
    {
        let to_fetch = list.dist_paths()
            // Fetch the information we need to create our requests for the release files.
            .filter_map(move |source_entry| {
                if source_entry.source {
                    return None;
                }

                Some(ReleaseFetch {
                    trusted: source_entry.options.iter().any(|x| x.as_str() == "trusted=yes"),
                    dist_path: source_entry.dist_path(),
                    components: source_entry.components.clone()
                })
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
            // - Handle trusted repos

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
                base_url: request.dist_path,
                components: request.components
            });

            let future = ReleaseFetched::new(future, keyring.clone())
                .validate_releases()
                .fetch_entries(client.clone(), archs.clone());

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
                    let release_file = SyncFile::open(&release_data.path)
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

                    Ok(ReleaseInfo {
                        base_url: release_data.base_url,
                        release,
                        components: release_data.components
                    })
                })
            })
        }
    }
}

pub struct ValidatedRelease<T: Future<Item = ReleaseInfo, Error = DistUpdateError> + Send> {
    future: T,
}

impl<T: Future<Item = ReleaseInfo, Error = DistUpdateError> + Send> ValidatedRelease<T> {
    pub fn fetch_entries(self, client: Arc<Client>, archs: Arc<Vec<Architecture>>)
        -> impl Future<Item = (), Error = DistUpdateError> + Send
    {
        self.future.and_then(|mut release_info| {
            let components = release_info.components;
            let release = release_info.release;
            if ! release_info.base_url.ends_with('/') {
                release_info.base_url.push('/');
            }

            let url: Arc<str> = Arc::from(release_info.base_url);

            let (crypto, entries) = match CHECKSUM_PRIORITY.iter()
                .filter_map(move |(checksum, kind)| {
                    release.sums.get(checksum.to_owned())
                        .cloned()
                        .map(|e| (kind, e))
                })
                .next()
                .ok_or_else(|| DistUpdateError::NoEntriesFound { repo: url.clone() })
            {
                Ok(v) => v,
                Err(why) => {
                    let err: Box<dyn Future<Item = (), Error = DistUpdateError> + Send> =
                        Box::new(futures::future::err(why));
                    return err;
                }
            };

            let entries = entries.filter_components(components)
                // Filter the sources which we don't need to request.
                .filter(move |(item, entries)| {
                    entries.first().map_or(false, |entry| {
                        entry.variant().map_or(false, |var| match var {
                            ReleaseVariant::Binary(arch) => archs.contains(&arch),
                            ReleaseVariant::Contents(arch) => archs.contains(&arch),
                            ReleaseVariant::Source => true,
                            _ => false
                        })
                    })
                })
                // Fetch the preferred paths and their checksums.
                .map(move |(item, entries)| {
                    // TODO: Optimize this.
                    let partial = entries.iter().find(|entry| entry.path.ends_with(".xz"))
                        .or_else(|| entries.iter().find(|entry| entry.path.ends_with(".gz")))
                        .or_else(|| entries.last())
                        .expect("no entries found for this release")
                        .clone();

                    let mut preferred = PreferredRequest {
                        checksum_decompressed: None,
                        checksum: partial.sum,
                        path: partial.path,
                        path_trim: 0,
                        size: partial.size,
                        compression: None,
                    };

                    if preferred.path != item {
                        let decompressed = entries.iter().find(|entry| entry.path == item)
                            .expect("decompressed entry not found");

                        preferred.checksum_decompressed = Some(decompressed.sum.clone());
                        preferred.compression = if preferred.path.ends_with(".xz") {
                            preferred.path_trim = 3;
                            Some(Compression::Xz)
                        } else if preferred.path.ends_with(".lz4") {
                            preferred.path_trim = 4;
                            Some(Compression::Lz4)
                        } else if preferred.path.ends_with(".gz") {
                            preferred.path_trim = 3;
                            Some(Compression::Gz)
                        } else {
                            panic!("unsupported compression: {:?}", preferred.path)
                        };
                    }

                    (preferred, crypto)
                })
                // Construct futures for fetching each file that is to be fetched.
                .map(move |(request, &crypto)| {
                    let fetch_url = [url.as_ref(), &request.path].concat();
                    let file: &str = match request.path.rfind('/') {
                        Some(pos) => &request.path[pos + 1..],
                        None => &request.path
                    };

                    let file_name = [&url[7..].replace("/", "_"), file].concat();

                    let dest = [LISTS, &file_name[..file_name.len() - request.path_trim as usize]].concat();
                    let partial_des = PathBuf::from([PARTIAL, &file_name].concat());

                    debug!("dest: {}\npartial: {:?}\nurl: {}", dest, partial_des, fetch_url);

                    let fetched_checksum: Arc<str> = Arc::from(request.checksum);
                    let dest_checksum: Arc<str> = Arc::from(
                        request.checksum_decompressed
                            .unwrap_or_else(|| fetched_checksum.as_ref().to_owned())
                    );

                    let fetch_request = AsyncFetcher::new(&client, fetch_url);

                    // Specify the checksum variant to use.
                    let fetch_request = match crypto {
                        ChecksumKind::Sha256 => {
                            fetch_request.request_to_path_with_checksum::<Sha256>(dest.into(), &dest_checksum)
                                .then_download(partial_des.clone())
                                .with_checksum::<Sha256>(fetched_checksum.clone())
                        }
                        ChecksumKind::Sha1 => {
                            fetch_request.request_to_path_with_checksum::<Sha1>(dest.into(), &dest_checksum)
                                .then_download(partial_des.clone())
                                .with_checksum::<Sha1>(fetched_checksum.clone())
                        }
                        ChecksumKind::Md5 => {
                            fetch_request.request_to_path_with_checksum::<Md5>(dest.into(), &dest_checksum)
                                .then_download(partial_des.clone())
                                .with_checksum::<Md5>(fetched_checksum.clone())
                        }
                    };

                    fn validate_destination_checksum<T: 'static + Future<Item = (), Error = FetchError> + Send>(
                        request: CompletedState<T>,
                        kind: ChecksumKind,
                        checksum: Arc<str>
                    ) -> Box<dyn Future<Item = (), Error = FetchError> + Send> {
                        match kind {
                            ChecksumKind::Sha256 => Box::new(request.with_destination_checksum::<Sha256>(checksum)),
                            ChecksumKind::Sha1 => Box::new(request.with_destination_checksum::<Sha1>(checksum)),
                            ChecksumKind::Md5 => Box::new(request.with_destination_checksum::<Md5>(checksum))
                        }
                    }

                    // Then specify the decoder that's required to decompress the archive.
                    let fetch_request = match request.compression {
                        Some(Compression::Xz) => {
                            validate_destination_checksum(
                                fetch_request.then_process(move |file| Ok(Box::new(XzDecoder::new(file)))),
                                crypto,
                                dest_checksum
                            )
                        }
                        Some(Compression::Gz) => {
                            validate_destination_checksum(
                                fetch_request.then_process(move |file| Ok(Box::new(GzDecoder::new(file)))),
                                crypto,
                                dest_checksum
                            )
                        }
                        _ => {
                            validate_destination_checksum(fetch_request.then_rename(), crypto, dest_checksum)
                        }
                    };

                    // Finally, make the error compatible with our program.
                    fetch_request.map_err(|why| DistUpdateError::Fetcher { why })
                });

            Box::new(futures::future::join_all(entries).map(|_| ()))
        })
    }

    pub fn into_future(self) -> T { self.future }
}

#[derive(Debug)]
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
    base_url: String,
    path: PathBuf,
    components: Vec<String>
}

pub struct ReleaseInfo {
    base_url: String,
    release: DistRelease,
    components: Vec<String>
}

#[derive(Debug)]
pub struct PreferredRequest {
    pub checksum_decompressed: Option<String>,
    pub checksum: String,
    pub path: String,
    pub path_trim: u8,
    pub size: u64,
    pub compression: Option<Compression>
}

#[derive(Copy, Clone, Debug, PartialEq)]
pub enum Compression {
    Gz,
    Xz,
    Lz4
}
