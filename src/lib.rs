extern crate deb_architectures;
extern crate deb_release_file;
extern crate apt_sources_lists;
extern crate failure;
#[macro_use]
extern crate failure_derive;
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
mod keyring;
mod status;
mod upgrade;

pub use self::dist::*;
pub use self::keyring::*;
pub use self::upgrade::*;
