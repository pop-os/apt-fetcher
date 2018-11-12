mod update;
mod upgrade;

pub use apt_sources_lists::*;
pub use self::update::*;
pub use self::upgrade::*;

/// Dist files which must exist within an apt repository's dists directory.
pub const REQUIRED_DIST_FILES: &[&str] = &["InRelease","Release","Release.gpg"];
