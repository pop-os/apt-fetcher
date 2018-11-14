use deb_architectures::{Architecture, supported_architectures};
use deb_release_file::{DistRelease, ReleaseEntry, ReleaseVariant};
use apt_sources_lists::*;
use decompressor::{Compression, Decompressor};
use keyring::AptKeyring;
use futures::{Future, Stream};
use gpgrv::verify_clearsign_armour;
use reqwest::async::{Client, Decoder, Response};
use std::io::{self, Error as TokioIoError, Seek, SeekFrom, Write};
use std::process::Command;
use tempfile_fast::PersistableTempFile;
use tokio::runtime::Runtime;
use tokio::fs::File;
use tokio::io::Read;
use status::StatusExt;
use std::sync::Arc;

pub type Url = Arc<String>;
pub type Data = Arc<String>;
pub type ReleasePath = String;

const APT_LISTS_CACHE: &str = "/var/lib/apt/lists/";
const PARTIAL: &str = "/var/lib/apt/lists/partial";
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
    LocalRepo { why: TokioIoError },
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

    pub fn tokio_update(&self) -> Result<(), DistUpdateError> {
        let architectures = supported_architectures()
            .map(Arc::new)
            .map_err(|why| DistUpdateError::Architectures { why })?;

        let mut runtime = Runtime::new()
            .map_err(|why| {
                DistUpdateError::Tokio { what: "constructing runtime", why }
            })?;

        // First, fetch all the release files and parse them.
        let requests = fetch_release_files(self.client.clone(), self.keyring.clone(), &self.sources_list);
        let release_files = futures::future::join_all(requests);
        let release_file_results = Arc::new(runtime.block_on(release_files)?);

        // Then, fetch all the files mentioned in those release files.
        // TODO: Write the temporary files to their permanent locations and set the file times.
        let futures = fetch_all_files(&self.client, &release_file_results, &architectures)?;
        let results = runtime.block_on(futures::future::join_all(futures))?;

        Ok(())
    }
}

fn fetch_all_files(
    client: &Arc<Client>,
    data: &[(Url, Data, DistRelease)],
    archs: &Arc<Vec<Architecture>>
) -> Result<
    Vec<impl Future<Item = (Url, ReleasePath, PersistableTempFile), Error = DistUpdateError> + Send + Send + 'static>,
    DistUpdateError
>{
    let releases = data.iter()
        // TODO: Write the release data once all of its entries have been written.
        .map(|(url, _data, release)| {
            let (crypto, entries) = CHECKSUM_PRIORITY.iter()
                .filter_map(move |&checksum| {
                    let checksum: String = checksum.to_owned();
                    release.sums.get(&checksum)
                        .cloned()
                        .map(|e| (checksum, e))
                })
                .next()
                .ok_or_else(|| DistUpdateError::NoEntriesFound { repo: url.clone() })?;

            let entries = entries.0.values().cloned().collect::<Vec<_>>();
            let futures = fetch_entries(client.clone(), archs.clone(), crypto, url.clone(), entries);

            Ok(futures)
        });

    let mut output = Vec::new();
    for release in releases {
        for data in release? {
            output.push(Box::new(data));
        }

    }

    Ok(output)
}

fn fetch_entries(
    client: Arc<Client>,
    archs: Arc<Vec<Architecture>>,
    crypto: String,
    url: Url,
    entries: Vec<Vec<ReleaseEntry>>,
) -> impl Iterator<Item = impl Future<Item = (Url, ReleasePath, PersistableTempFile), Error = DistUpdateError> + Send> {
    entries.into_iter()
        // Pick the best option to fetch, favoring xz over gz.
        .map(|entries| {
            entries.iter().find(|entry| entry.path.ends_with(".xz"))
                .or_else(|| entries.iter().find(|entry| entry.path.ends_with(".gz")))
                .or_else(|| entries.last())
                .expect("no entries found for this release")
                .clone()
        })
        // Filter the architectures which we don't need to request.
        .filter(move |entry| entry.variant().map_or(false, |var| match var {
            ReleaseVariant::Binary(arch) => archs.contains(&arch),
            ReleaseVariant::Contents(arch) => archs.contains(&arch),
            ReleaseVariant::Source => true,
            _ => false
        }))
        // The filename will point out the compression format to decode with.
        .map(move |entry| {
            let compression = if entry.path.ends_with(".xz") {
                Compression::Xz
            } else if entry.path.ends_with(".lz4") {
                Compression::Lz4
            } else if entry.path.ends_with(".gz") {
                Compression::Gz
            } else {
                Compression::None
            };

            (entry, compression)
        })
        // Fetch the request and store it into a temporary file.
        .map(move |(entry, compression)| {
            let from = [url.as_str(), entry.path.as_str()].concat();
            let url = url.clone();

            // TODO: Handle local files as well as HTTP requests.
            get_request(&client, from)
                // TODO: Write to a temporary file instead of in memory.
                // TODO: Check whether we need to download it or not.
                .and_then(|resp| decode_to_vec(resp.into_body()))
                // TODO: Validate that the file fetched has the correct checksum and size.
                .and_then(move |c| {
                    Decompressor::from_variant(io::Cursor::new(c), compression)
                        .map_err(|why| DistUpdateError::Decompressor { why })
                })
                .map(move |mut decompressor| {
                    let path = entry.path.clone();
                    let mut file = PersistableTempFile::new_in(PARTIAL).unwrap();
                    io::copy(&mut decompressor, &mut file).unwrap();
                    eprintln!("{} was fetched and decompressed", path);
                    (url, path, file)
                })
        })
}

/// Construct an iterator of futures for fetching each dist release file of each source.
fn fetch_release_files(
    client: Arc<Client>,
    keyring: Option<Arc<AptKeyring>>,
    list: &SourcesList,
) -> impl Iterator<Item = impl Future<Item = (Url, Data, DistRelease), Error = DistUpdateError> + Send> {
    let paths = list.dist_paths().cloned().collect::<Vec<_>>();

    paths.into_iter()
        // TODO: filter release files that have already been fetched.
        .map(move |entry| {
            let release_file = fetch_release_file(&client, keyring.clone(), entry.dist_path());

            release_file.and_then(|(url, release_file)| {
                String::from_utf8(release_file)
                    .map_err(|_| DistUpdateError::InvalidUtf8)
                    .and_then(|string| {
                        string.parse::<DistRelease>()
                            .map_err(|why| DistUpdateError::InvalidReleaseData { why })
                            .map(|r| (Arc::new(url), Arc::new(string), r))
                    })
            })
        })
}

/// Construct an iterator of futures for fetching each dist release file.
///
/// # Keyrings
///
/// If a keyring exists, we will fetch the "InRelease" file and check the signature against
/// known keys in the keyring. Otherwise, we will fetch the "Release" file instead.
///
/// # Implemented protocols
///
/// - If the release file comes from a file, `tokio::fs::File` will be used.
/// - If it is a HTTP/HTTPS URL, `reqwest::async::Client` is used.
fn fetch_release_file(
    client: &Arc<Client>,
    keyring: Option<Arc<AptKeyring>>,
    mut url: String
) -> impl Future<Item = (String, Vec<u8>), Error = DistUpdateError> + Send {
    if ! url.ends_with('/') {
        url.push('/')
    }

    let future: Box<dyn Future<Item = Vec<u8>, Error = DistUpdateError> + Send> = match keyring {
        Some(keyring) => {
            let inrelease_path = [url.as_str(), "InRelease"].concat();

            if inrelease_path.starts_with("file://") {
                let inrelease_decoded = read_file(inrelease_path[7..].to_owned());

                let inrelease = validate_gpg_signature(inrelease_decoded, keyring);
                Box::new(inrelease)
            } else {
                let inrelease_decoded = get_request(&client, inrelease_path)
                    .and_then(|resp| decode_to_vec(resp.into_body()));

                let inrelease = validate_gpg_signature(inrelease_decoded, keyring);
                Box::new(inrelease)
            }
        }
        None => {
            let release_path = [url.as_str(), "Release"].concat();

            if release_path.starts_with("file://") {
                Box::new(read_file(release_path[7..].to_owned()))
            } else {
                let release = get_request(&client, release_path)
                    .and_then(|resp| decode_to_vec(resp.into_body()));
                Box::new(release)
            }
        }
    };

    future.map(|r| (url, r))
}

fn get_request(client: &Arc<Client>, url: String) -> impl Future<Item = Response, Error = DistUpdateError> + Send {
    client.get(&url)
        .send()
        .inspect(move |_v| {
            eprintln!("GET {}", url);
        })
        .and_then(|resp| resp.error_for_status())
        .map_err(|why| DistUpdateError::Request { why })
}

fn read_file(path: String) -> impl Future<Item = Vec<u8>, Error = DistUpdateError> + Send {
    File::open(path)
        .and_then(|mut file| {
            let mut release_file = Vec::new();
            file.read_to_end(&mut release_file)?;
            Ok(release_file)
        })
        .map_err(|why| DistUpdateError::LocalRepo { why })
}

/// Take a reqwest::Decoder and convert it into a future which collects it into a `Vec<u8>`
fn decode_to_vec(decoder: Decoder) -> impl Future<Item = Vec<u8>, Error = DistUpdateError> + Send {
    decoder
        .concat2()
        .map(|chunk| {
            eprintln!("finished decoding");
            let chunk: &[u8] = &chunk;
            Vec::from(chunk)
        })
        .map_err(|why| io::Error::new(
            io::ErrorKind::Other,
            format!("failed to fetch: {}", why)
        ))
        .map_err(|why| DistUpdateError::Decoder { why })
}

/// Validate the GPG signature from the given input.
fn validate_gpg_signature<I>(
    input: I,
    keyring: Arc<AptKeyring>
) -> impl Future<Item = Vec<u8>, Error = DistUpdateError> + Send
    where I: Future<Item = Vec<u8>, Error = DistUpdateError> + Send
{
    input
        .and_then(move |decoded| {
            let mut output = io::Cursor::new(Vec::new());
            verify_clearsign_armour(&mut io::Cursor::new(decoded), &mut output, &keyring)
                .map_err(|why| DistUpdateError::GpgValidation { why })?;

            Ok(output.into_inner())
        })

}
