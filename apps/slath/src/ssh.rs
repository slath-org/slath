// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! SSH helpers run by OpenSSH's sshd.
//!
//! - `slath ssh-keys <fingerprint>` is the `AuthorizedKeysCommand`. It prints
//!   one `authorized_keys` line for a known key, forcing `slath ssh-serve`
//!   with all forwarding and TTYs disabled (`restrict`), or nothing at all.
//! - `slath ssh-serve --key-id <id>` is that forced command. It accepts only
//!   Git transport commands, and hands them with the SSH channel's stdin,
//!   stdout, and stderr to `slath serve`, which authorizes the request and
//!   runs Git on them.
//!
//! Neither touches the database, the repositories, or any server file; both
//! only talk to `slath serve` over the IPC socket, so the SSH login can be
//! a user of its own. When the server can't be reached they deny.

use std::io::Write;
use std::os::fd::AsFd;
use std::path::Path;
use std::process::ExitCode;

use slath_git::ipc::{self, Request};
use slath_git::ssh::{SshService, parse_command};
use slath_git::transport::Service;

/// Default IPC socket path (`<run_dir>/slath.sock`).
pub const DEFAULT_SOCKET: &str = "/run/slath/slath.sock";

/// True if `s` can appear in an sshd `command="…"` option unquoted and
/// unescaped (sshd hands that string to a shell, so it must be inert).
fn is_inert(s: &str) -> bool {
    !s.is_empty()
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._/-".contains(&b))
}

fn fail(msg: &str) -> ExitCode {
    let _ = writeln!(std::io::stderr(), "slath: {msg}");
    ExitCode::FAILURE
}

pub fn keys(socket: &Path, fingerprint: &str) -> ExitCode {
    let resp = match ipc::call(
        socket,
        &Request::LookupKey {
            fingerprint: fingerprint.to_owned(),
        },
        ipc::HOOK_TIMEOUT,
    ) {
        Ok(r) => r,
        // sshd treats a failing command like "no keys": access denied.
        Err(e) => return fail(&e.to_string()),
    };
    let Some(key) = resp.key else {
        return ExitCode::SUCCESS;
    };
    let exe = match std::env::current_exe().and_then(std::fs::canonicalize) {
        Ok(p) => p,
        Err(e) => return fail(&format!("cannot locate the slath binary: {e}")),
    };
    let (Some(exe), Some(sock)) = (exe.to_str(), socket.to_str()) else {
        return fail("binary and socket paths must be UTF-8");
    };
    let b64_ok = key
        .key_base64
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b"+/=".contains(&b));
    if !(is_inert(exe)
        && is_inert(sock)
        && is_inert(&key.key_id)
        && is_inert(&key.key_type)
        && b64_ok)
    {
        return fail("refusing to print an unsafe authorized_keys line");
    }
    println!(
        "restrict,command=\"{exe} ssh-serve --socket {sock} --key-id {}\" {} {}",
        key.key_id, key.key_type, key.key_base64
    );
    ExitCode::SUCCESS
}

pub fn serve(socket: &Path, key_id: &str) -> ExitCode {
    let Some(cmd) = std::env::var("SSH_ORIGINAL_COMMAND")
        .ok()
        .as_deref()
        .and_then(parse_command)
    else {
        return fail("unsupported command (Slath only provides Git access over SSH)");
    };
    let service = match cmd.service {
        SshService::UploadPack => Service::UploadPack,
        SshService::ReceivePack => Service::ReceivePack,
        SshService::UploadArchive => return fail("git-upload-archive is not supported"),
    };
    let request = Request::SshSession {
        key_id: key_id.to_owned(),
        service: service.name().to_owned(),
        namespace: cmd.namespace,
        repo: cmd.repo,
        git_protocol: std::env::var("GIT_PROTOCOL").ok(),
    };
    let (stdin, stdout, stderr) = (std::io::stdin(), std::io::stdout(), std::io::stderr());
    let stdio = [stdin.as_fd(), stdout.as_fd(), stderr.as_fd()];
    let resp = match ipc::ssh_session(socket, &request, stdio, ipc::HOOK_TIMEOUT) {
        Ok(r) => r,
        Err(e) => return fail(&e.to_string()),
    };
    match resp.exit_code {
        Some(0) => ExitCode::SUCCESS,
        Some(code) => ExitCode::from(u8::try_from(code).unwrap_or(1).max(1)),
        None => {
            let mut stderr = std::io::stderr();
            for m in &resp.messages {
                let _ = writeln!(stderr, "{m}");
            }
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inert_strings() {
        assert!(is_inert("/usr/bin/slath"));
        assert!(is_inert("0192f1a2-3b4c-7d5e-8f60-718293a4b5c6"));
        for bad in ["", "a b", "a\"b", "a;b", "$(x)", "a\\b", "a\nb", "a'b"] {
            assert!(!is_inert(bad), "{bad:?}");
        }
    }
}
