// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! SSH helpers run by OpenSSH's sshd.
//!
//! - `slath ssh-keys <fingerprint>` is the `AuthorizedKeysCommand`. It prints
//!   one `authorized_keys` line for a known key, forcing `slath ssh-serve`
//!   with all forwarding and TTYs disabled (`restrict`), or nothing at all.
//! - `slath ssh-serve --key-id <id>` is that forced command. It accepts only
//!   Git transport commands for an authorized repository, then runs Git with
//!   the hardened runner on the SSH channel's stdio.
//!
//! Neither touches the database; both ask `slath serve` over the IPC socket.
//! When the server can't be reached they deny.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use slath_git::ipc::{self, Request};
use slath_git::ssh::{SshService, parse_command};
use slath_git::transport::{self, HookEnv, Service};
use slath_git::{Runner, RunnerConfig};

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
    let request = Request::AuthorizeTransport {
        key_id: key_id.to_owned(),
        service: service.name().to_owned(),
        namespace: cmd.namespace,
        repo: cmd.repo,
    };
    let resp = match ipc::call(socket, &request, ipc::HOOK_TIMEOUT) {
        Ok(r) => r,
        Err(e) => return fail(&e.to_string()),
    };
    let Some(grant) = resp.transport.filter(|_| resp.allow) else {
        let mut stderr = std::io::stderr();
        for m in &resp.messages {
            let _ = writeln!(stderr, "{m}");
        }
        return ExitCode::FAILURE;
    };
    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => return fail(&e.to_string()),
    };
    rt.block_on(async {
        let mut rc = RunnerConfig::new(grant.git_binary.into(), grant.home_dir.into());
        rc.hooks_dir = grant.hooks_dir.map(PathBuf::from);
        rc.max_concurrent = 1;
        let runner = match Runner::new(rc) {
            Ok(r) => r,
            Err(e) => return fail(&e.to_string()),
        };
        let git_protocol = std::env::var("GIT_PROTOCOL").ok();
        let hooks = grant.hook_token.as_deref().map(|token| HookEnv {
            token,
            ipc_socket: socket,
        });
        let git_dir = PathBuf::from(&grant.git_dir);
        let mut child = match transport::spawn_ssh(
            &runner,
            &git_dir,
            service,
            git_protocol.as_deref(),
            hooks.as_ref(),
        )
        .await
        {
            Ok(c) => c,
            Err(e) => return fail(&e.to_string()),
        };
        match child.wait().await {
            Ok(status) if status.success() => ExitCode::SUCCESS,
            Ok(status) => ExitCode::from(u8::try_from(status.code().unwrap_or(1)).unwrap_or(1)),
            Err(e) => fail(&e.to_string()),
        }
    })
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
