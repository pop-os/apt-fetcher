#![recursion_limit="128"]

#[macro_use] extern crate err_derive;
#[macro_use] extern crate log;

extern crate apt_keyring as keyring;
extern crate apt_release_file;
extern crate apt_sources_lists;
extern crate async_fetcher;
extern crate deb_architectures;
extern crate failure;
extern crate filetime;
extern crate flate2;
extern crate futures;
extern crate gpgrv;
extern crate lz4;
extern crate md5;
extern crate reqwest;
extern crate sha1;
extern crate sha2;
extern crate tempfile_fast;
extern crate tokio;
extern crate tokio_threadpool;
extern crate xz2;

pub mod apt_uris;
mod command;
mod dist;
mod status;

pub use self::dist::*;
