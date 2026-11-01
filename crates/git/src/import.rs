// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Fetching a repository from another server into a new, empty one.
//!
//! Only branches and tags are fetched. The remote's address was checked by
//! the caller and is pinned (`http.curloptResolve`), so Git never resolves
//! the name again; redirects aren't followed (they would reach an unchecked
//! host), bundle URIs are ignored, every object is checked (`fsckObjects`),
//! and submodules aren't touched. The fetch is killed when it runs past
//! its timeout or the repository grows beyond the size limit.

use std::net::IpAddr;
use std::path::Path;
use std::time::Duration;

use crate::error::{GitError, LimitKind};
use crate::runner::Runner;

/// Where to fetch from.
#[derive(Debug, Clone)]
pub struct Remote<'a> {
    /// The URL without credentials (`https://host[:port]/path`).
    pub url: &'a str,
    pub host: &'a str,
    pub port: u16,
    /// The checked address the connection must use.
    pub ip: IpAddr,
    /// Plain `http://` (only when the administrator allowed it).
    pub http: bool,
    /// A Git config file with credentials (`http.extraHeader`), if any.
    pub credentials: Option<&'a Path>,
}

fn dir_size(path: &Path) -> u64 {
    let mut total = 0;
    let mut stack = vec![path.to_path_buf()];
    while let Some(p) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&p) else {
            continue;
        };
        for e in rd.flatten() {
            match e.metadata() {
                Ok(m) if m.is_dir() => stack.push(e.path()),
                Ok(m) => total += m.len(),
                Err(_) => {}
            }
        }
    }
    total
}

/// Fetches all branches and tags of `remote` into the bare repository at
/// `git_dir`.
pub async fn fetch(
    runner: &Runner,
    git_dir: &Path,
    remote: &Remote<'_>,
    timeout: Duration,
    max_bytes: u64,
) -> Result<(), GitError> {
    let host = match remote.ip {
        IpAddr::V6(v6) => format!("[{v6}]"),
        IpAddr::V4(v4) => v4.to_string(),
    };
    let mut inv = runner
        .repo(git_dir)
        .config("protocol.https.allow", "always")
        .config("http.followRedirects", "false")
        .config("transfer.bundleURI", "false")
        .config("fetch.fsckObjects", "true")
        .config("transfer.fsckObjects", "true")
        .config("submodule.recurse", "false")
        .config("fetch.recurseSubmodules", "false")
        .config("credential.helper", "")
        .config("core.askPass", "")
        .config("http.lowSpeedLimit", "1000")
        .config("http.lowSpeedTime", "60")
        .config(
            "http.curloptResolve",
            format!("{}:{}:{host}", remote.host, remote.port),
        );
    if remote.http {
        inv = inv.config("protocol.http.allow", "always");
    }
    if let Some(file) = remote.credentials {
        inv = inv.global_config_file(file);
    }
    let inv = inv
        .args([
            "fetch",
            "--quiet",
            "--no-tags",
            "--no-write-fetch-head",
            "--no-recurse-submodules",
            "--end-of-options",
            remote.url,
            "+refs/heads/*:refs/heads/*",
            "+refs/tags/*:refs/tags/*",
        ])
        .timeout(timeout);
    let objects = git_dir.join("objects");
    let watchdog = async {
        loop {
            tokio::time::sleep(Duration::from_millis(500)).await;
            let objects = objects.clone();
            let size = tokio::task::spawn_blocking(move || dir_size(&objects))
                .await
                .unwrap_or(0);
            if size > max_bytes {
                return;
            }
        }
    };
    tokio::select! {
        out = inv.run() => out.map(|_| ()),
        // Dropping the fetch kills its process group.
        () = watchdog => Err(GitError::Limit(LimitKind::Output)),
    }
}
