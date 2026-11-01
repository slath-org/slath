// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Spawning `upload-pack`/`receive-pack` for smart HTTP and SSH.

use std::os::fd::OwnedFd;
use std::path::Path;

use crate::error::GitError;
use crate::runner::{GitChild, Runner};

/// Maximum size of a ref advertisement we buffer for `info/refs`.
pub const MAX_ADVERTISEMENT: usize = 64 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Service {
    UploadPack,
    ReceivePack,
}

impl Service {
    /// Parses `git-upload-pack` / `git-receive-pack`.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "git-upload-pack" => Some(Self::UploadPack),
            "git-receive-pack" => Some(Self::ReceivePack),
            _ => None,
        }
    }

    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::UploadPack => "git-upload-pack",
            Self::ReceivePack => "git-receive-pack",
        }
    }

    const fn subcommand(self) -> &'static str {
        match self {
            Self::UploadPack => "upload-pack",
            Self::ReceivePack => "receive-pack",
        }
    }

    #[must_use]
    pub const fn is_write(self) -> bool {
        matches!(self, Self::ReceivePack)
    }
}

/// Environment for the push hooks of a `receive-pack` run.
#[derive(Debug, Clone)]
pub struct HookEnv<'a> {
    pub token: &'a str,
    pub ipc_socket: &'a Path,
    /// `receive.maxInputSize`: the largest pack the push may send.
    pub max_input_bytes: Option<u64>,
}

/// Formats one pkt-line.
#[must_use]
pub fn pkt_line(data: &[u8]) -> Vec<u8> {
    let mut out = format!("{:04x}", data.len() + 4).into_bytes();
    out.extend_from_slice(data);
    out
}

/// True if a `GIT_PROTOCOL` value requests protocol v2.
#[must_use]
pub fn wants_v2(git_protocol: Option<&str>) -> bool {
    git_protocol.is_some_and(|p| p.split(':').any(|kv| kv == "version=2"))
}

fn invocation(
    runner: &Runner,
    git_dir: &Path,
    service: Service,
    git_protocol: Option<&str>,
    hooks: Option<&HookEnv<'_>>,
) -> crate::runner::Invocation {
    let mut inv = match service {
        Service::UploadPack => runner.repo(git_dir),
        Service::ReceivePack => runner.receive_pack(git_dir),
    };
    if let Some(p) = git_protocol {
        inv = inv.git_protocol(p);
    }
    if let Some(h) = hooks {
        inv = inv.hook_env(h.token, h.ipc_socket);
        if let Some(max) = h.max_input_bytes {
            inv = inv.config("receive.maxInputSize", max);
        }
    }
    inv.arg(service.subcommand())
}

/// The ref advertisement for `GET info/refs` (smart HTTP), including the
/// `# service=…` preamble unless the client asked for protocol v2.
pub async fn http_info_refs(
    runner: &Runner,
    git_dir: &Path,
    service: Service,
    git_protocol: Option<&str>,
) -> Result<Vec<u8>, GitError> {
    let out = invocation(runner, git_dir, service, git_protocol, None)
        .args(["--stateless-rpc", "--http-backend-info-refs"])
        .arg(git_dir)
        .max_output(MAX_ADVERTISEMENT)
        .run()
        .await?;
    let mut body = Vec::new();
    if !wants_v2(git_protocol) {
        body.extend(pkt_line(
            format!("# service={}\n", service.name()).as_bytes(),
        ));
        body.extend_from_slice(b"0000");
    }
    body.extend(out.stdout);
    Ok(body)
}

/// Spawns a stateless-RPC `upload-pack`/`receive-pack` for one smart HTTP
/// POST. The caller streams the request into stdin and stdout back out.
pub async fn spawn_http_rpc(
    runner: &Runner,
    git_dir: &Path,
    service: Service,
    git_protocol: Option<&str>,
    hooks: Option<&HookEnv<'_>>,
) -> Result<GitChild, GitError> {
    invocation(runner, git_dir, service, git_protocol, hooks)
        .arg("--stateless-rpc")
        .arg(git_dir)
        .spawn()
        .await
}

/// Spawns a full-duplex `upload-pack`/`receive-pack` for SSH, attached to
/// the SSH channel's stdin, stdout, and stderr.
pub async fn spawn_ssh(
    runner: &Runner,
    git_dir: &Path,
    service: Service,
    git_protocol: Option<&str>,
    hooks: Option<&HookEnv<'_>>,
    stdio: [OwnedFd; 3],
) -> Result<GitChild, GitError> {
    invocation(runner, git_dir, service, git_protocol, hooks)
        .arg(git_dir)
        .spawn_attached(stdio)
        .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pkt_lines() {
        assert_eq!(
            pkt_line(b"# service=git-upload-pack\n"),
            b"001e# service=git-upload-pack\n"
        );
    }

    #[test]
    fn v2_detection() {
        assert!(wants_v2(Some("version=2")));
        assert!(wants_v2(Some("object-format=sha1:version=2")));
        assert!(!wants_v2(Some("version=1")));
        assert!(!wants_v2(None));
    }
}
