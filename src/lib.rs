#[macro_use] extern crate failure_derive;
#[macro_use] extern crate log;

extern crate apt_keyring as keyring;
extern crate apt_sources_lists;
extern crate async_fetcher;
extern crate deb_architectures;
extern crate deb_release_file;
extern crate failure;
extern crate filetime;
extern crate futures;
extern crate gpgrv;
extern crate libflate;
extern crate lz4;
extern crate reqwest;
extern crate tempfile_fast;
extern crate tokio;
extern crate xz2;

mod decompressor;
mod dist;
mod status;
mod upgrade;

pub use self::dist::*;
pub use self::upgrade::*;
