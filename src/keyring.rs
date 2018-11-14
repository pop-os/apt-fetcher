use gpgrv::Keyring;
use std::io;
use std::fs::{self, File};
use std::ops::Deref;
use std::path::Path;

pub struct AptKeyring(Keyring);

impl AptKeyring {
    pub fn new() -> io::Result<Self> {
        let mut keyring = AptKeyring(Keyring::new());
        keyring.append_keys_from("/etc/apt/trusted.gpg")?;

        for entry in fs::read_dir("/etc/apt/trusted.gpg.d/")? {
            let entry = entry?;
            keyring.append_keys_from(&entry.path())?;
        }

        Ok(keyring)
    }

    pub fn append_keys_from<P: AsRef<Path>>(&mut self, path: P) -> io::Result<usize> {
        let path = path.as_ref();
        self.0.append_keys_from(File::open(path)?)
            .map_err(|why| io::Error::new(
                io::ErrorKind::Other,
                format!("failed to add keys from keyring at {}: {}", path.display(), why)
            ))
    }
}

impl Deref for AptKeyring {
    type Target = Keyring;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
