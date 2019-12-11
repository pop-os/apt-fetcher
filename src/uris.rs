use async_std::{fs::File, io::BufReader, prelude::*};

use anyhow::Context;
use futures::{channel::mpsc::Sender, SinkExt};
use os_pipe::pipe;
use pidfd::PidFd;

use std::{
    hash::{Hash, Hasher},
    io::Error as IoError,
    os::unix::io::{AsRawFd, FromRawFd},
    process::{Command, Stdio},
    str::FromStr,
};

#[derive(Debug, Error)]
pub enum Error {
    #[error("apt command failed: {}", _0)]
    Command(IoError),
    #[error("uri not found in output: {}", _0)]
    UriNotFound(Box<str>),
    #[error("invalid URI value: {}", _0)]
    UriInvalid(Box<str>),
    #[error("name not found in output: {}", _0)]
    NameNotFound(Box<str>),
    #[error("size not found in output: {}", _0)]
    SizeNotFound(Box<str>),
    #[error("size in output could not be parsed as an integer: {}", _0)]
    SizeParse(Box<str>),
    #[error("md5sum not found in output: {}", _0)]
    Md5NotFound(Box<str>),
    #[error("md5 prefix (MD5Sum:) not found in md5sum: {}", _0)]
    Md5Prefix(Box<str>),
}

#[derive(Clone, Debug)]
pub struct DebUri {
    pub uri: Box<str>,
    pub name: Box<str>,
    pub size: u64,
    pub md5sum: Box<str>,
}

/// Fetches package URIs which are in need of being updated.
pub async fn update_uris(mut packages: Sender<DebUri>) -> anyhow::Result<()> {
    debug!("fetching debian package URIs from apt-get child process");

    let (rx, tx) = pipe().context("failed to spawn an OS pipe")?;

    // Fetches a list of URIs from apt-get
    let child_process = async move {
        Command::new("apt-get")
            .env("DEBIAN_FRONTEND", "noninteractive")
            .arg("--print-uris")
            .arg("full-upgrade")
            .stderr(Stdio::null())
            .stdin(Stdio::null())
            .stdout(tx)
            .spawn()
            .map(|child| PidFd::from(&child))
            .context("failed to spawn apt-get child process")?
            .into_future()
            .await
            .context("failed to get the exit status of the apt-get child process")
    };

    // Parses the output from apt-get as it is written.
    let parser = async move {
        let line_reader = &mut BufReader::new(unsafe { File::from_raw_fd(rx.as_raw_fd()) });
        let line = &mut String::new();
        let mut read: usize;

        loop {
            line.clear();
            read = line_reader
                .read_line(line)
                .await
                .context("failed to read line from apt-get")?;

            if read == 0 {
                break;
            }

            if !line.starts_with('\'') {
                continue;
            }

            let uri = line.parse::<DebUri>().context("line parsing error")?;
            let _ = packages.send(uri).await;
        }

        Ok(())
    };

    let (_, packages) = try_join!(child_process, parser)?;

    debug!("fetching complete without error");

    Ok(packages)
}

impl PartialEq for DebUri {
    fn eq(&self, other: &Self) -> bool {
        self.uri == other.uri
    }
}

impl Hash for DebUri {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.uri.hash(state);
    }
}

impl FromStr for DebUri {
    type Err = Error;

    fn from_str(line: &str) -> Result<Self, Self::Err> {
        let mut words = line.split_whitespace();

        let mut uri = words
            .next()
            .ok_or_else(|| Error::UriNotFound(line.into()))?;

        // We need to remove the single quotes that apt-get encloses the URI within.
        if uri.len() <= 3 {
            return Err(Error::UriInvalid(uri.into()));
        } else {
            uri = &uri[1..uri.len() - 1];
        }

        let name = words
            .next()
            .ok_or_else(|| Error::NameNotFound(line.into()))?;
        let size = words
            .next()
            .ok_or_else(|| Error::SizeNotFound(line.into()))?;
        let size = size
            .parse::<u64>()
            .map_err(|_| Error::SizeParse(size.into()))?;
        let mut md5sum = words
            .next()
            .ok_or_else(|| Error::Md5NotFound(line.into()))?;

        if md5sum.starts_with("MD5Sum:") {
            md5sum = &md5sum[7..];
        } else {
            return Err(Error::Md5Prefix(md5sum.into()));
        }

        Ok(DebUri {
            uri: uri.into(),
            name: name.into(),
            size,
            md5sum: md5sum.into(),
        })
    }
}
