use apt_sources_lists::*;
use dist::REQUIRED_DIST_FILES;
use futures::Future;
use reqwest::async::{Client, Response};
use std::io;
use std::process::Command;
use tokio::runtime::current_thread::Runtime;
use status::StatusExt;

#[derive(Debug, Fail)]
pub enum DistUpdateError {
    #[fail(display = "tokio error: failure {}: {}", what, why)]
    Tokio { what: &'static str, why: tokio::io::Error },
    #[fail(display = "http/s request failed: {}", why)]
    Request { why: reqwest::Error },
    #[fail(display = "failed to exec 'apt update': {}", why)]
    AptUpdate { why: io::Error }
}

pub struct Updater<'a> {
    client: &'a Client,
    sources_list: &'a SourcesList
}

impl<'a> Updater<'a> {
    pub fn new(client: &'a Client, sources_list: &'a SourcesList) -> Self {
        Self { client, sources_list }
    }

    pub fn update(&self) -> Result<(), DistUpdateError> {
        Command::new("apt")
            .args(&["update"])
            .status()
            .and_then(StatusExt::as_result)
            .map_err(|why| DistUpdateError::AptUpdate { why })
    }
}
