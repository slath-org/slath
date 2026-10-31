// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! `slath hook pre-receive|post-receive`, run by Git during a push.
//!
//! The hook forwards the ref updates to `slath serve` over IPC. pre-receive
//! fails closed: if the server can't be reached or doesn't answer within
//! five seconds, the push is denied. post-receive never fails the
//! push; the reconciler repairs anything it misses.

use std::ffi::OsStr;
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::Subcommand;
use slath_git::hookipc::{self, HOOK_TIMEOUT, Quarantine, RefUpdate, Request};

#[derive(Debug, Clone, Copy, Subcommand)]
pub enum HookKind {
    PreReceive,
    PostReceive,
}

impl HookKind {
    pub fn from_argv0(argv0: &OsStr) -> Option<Self> {
        match Path::new(argv0).file_name()?.to_str()? {
            "pre-receive" => Some(Self::PreReceive),
            "post-receive" => Some(Self::PostReceive),
            _ => None,
        }
    }
}

fn env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.is_empty())
}

fn read_updates() -> Result<Vec<RefUpdate>, String> {
    let stdin = std::io::stdin();
    let mut updates = Vec::new();
    for line in stdin.lock().split(b'\n') {
        let line = line.map_err(|e| e.to_string())?;
        if line.is_empty() {
            continue;
        }
        updates.push(RefUpdate::parse_line(&line).ok_or("malformed ref update from git")?);
    }
    Ok(updates)
}

pub fn run(kind: HookKind) -> ExitCode {
    let mut stderr = std::io::stderr();
    let result = (|| {
        let token = env("SLATH_HOOK_TOKEN").ok_or("missing SLATH_HOOK_TOKEN")?;
        let socket = PathBuf::from(env("SLATH_IPC").ok_or("missing SLATH_IPC")?);
        let updates = read_updates()?;
        let request = match kind {
            HookKind::PreReceive => Request::PreReceive {
                token,
                updates,
                quarantine: Quarantine {
                    object_directory: env("GIT_OBJECT_DIRECTORY"),
                    alternate_object_directories: env("GIT_ALTERNATE_OBJECT_DIRECTORIES"),
                    quarantine_path: env("GIT_QUARANTINE_PATH"),
                },
            },
            HookKind::PostReceive => Request::PostReceive { token, updates },
        };
        hookipc::call(&socket, &request, HOOK_TIMEOUT).map_err(|e| e.to_string())
    })();
    match (kind, result) {
        (_, Ok(resp)) => {
            for m in &resp.messages {
                let _ = writeln!(stderr, "{m}");
            }
            if resp.allow || matches!(kind, HookKind::PostReceive) {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            }
        }
        (HookKind::PreReceive, Err(e)) => {
            let _ = writeln!(stderr, "slath: push rejected: {e}");
            ExitCode::FAILURE
        }
        (HookKind::PostReceive, Err(e)) => {
            let _ = writeln!(
                stderr,
                "slath: warning: post-receive failed ({e}); it will be reconciled"
            );
            ExitCode::SUCCESS
        }
    }
}
