// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! `slath hook pre-receive|proc-receive|post-receive`, run by Git during a
//! push.
//!
//! The hook forwards the ref updates to `slath serve` over IPC. pre-receive
//! fails closed: if the server can't be reached or doesn't answer within
//! five seconds, the push is denied. proc-receive (pushes to `refs/for/…`,
//! which the server turns into branch updates) refuses every command it
//! couldn't hand over. post-receive never fails the push; the reconciler
//! repairs anything it misses.

use std::ffi::OsStr;
use std::io::{BufRead, Read, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::Subcommand;
use slath_git::ipc::{self, HOOK_TIMEOUT, ProcResult, Quarantine, RefUpdate, Request};

/// Git's hook names, which are also the subcommands.
#[derive(Debug, Clone, Copy, Subcommand)]
#[allow(clippy::enum_variant_names)]
pub enum HookKind {
    PreReceive,
    ProcReceive,
    PostReceive,
}

impl HookKind {
    pub fn from_argv0(argv0: &OsStr) -> Option<Self> {
        match Path::new(argv0).file_name()?.to_str()? {
            "pre-receive" => Some(Self::PreReceive),
            "proc-receive" => Some(Self::ProcReceive),
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

/// The longest pkt-line Git sends (65520 bytes with its length).
const MAX_PKT: usize = 65520;

/// Reads one pkt-line: `Some(payload)`, or `None` for a flush packet.
fn read_pkt(input: &mut impl Read) -> Result<Option<Vec<u8>>, String> {
    let mut len = [0u8; 4];
    input.read_exact(&mut len).map_err(|e| e.to_string())?;
    let len = std::str::from_utf8(&len)
        .ok()
        .and_then(|l| usize::from_str_radix(l, 16).ok())
        .ok_or("malformed pkt-line from git")?;
    match len {
        0 => Ok(None),
        1..=4 => Err("malformed pkt-line from git".to_owned()),
        n if n > MAX_PKT => Err("pkt-line too long".to_owned()),
        n => {
            let mut data = vec![0u8; n - 4];
            input.read_exact(&mut data).map_err(|e| e.to_string())?;
            if data.last() == Some(&b'\n') {
                data.pop();
            }
            Ok(Some(data))
        }
    }
}

fn write_pkt(out: &mut impl Write, data: &[u8]) -> std::io::Result<()> {
    out.write_all(&slath_git::transport::pkt_line(data))
}

/// proc-receive: negotiates version 1 without features, reads the
/// commands, has the server carry them out, and reports each result.
fn proc_receive(stderr: &mut impl Write) -> ExitCode {
    let mut input = std::io::stdin().lock();
    let mut output = std::io::stdout().lock();
    let commands = (|| {
        let version = read_pkt(&mut input)?.ok_or("git sent no version")?;
        if !version.starts_with(b"version=1") {
            return Err("unsupported proc-receive version".to_owned());
        }
        while read_pkt(&mut input)?.is_some() {}
        write_pkt(&mut output, b"version=1\0")
            .and_then(|()| output.write_all(b"0000"))
            .and_then(|()| output.flush())
            .map_err(|e| e.to_string())?;
        let mut updates = Vec::new();
        while let Some(line) = read_pkt(&mut input)? {
            updates.push(RefUpdate::parse_line(&line).ok_or("malformed command from git")?);
        }
        Ok(updates)
    })();
    let updates = match commands {
        Ok(u) => u,
        Err(e) => {
            let _ = writeln!(stderr, "slath: push rejected: {e}");
            return ExitCode::FAILURE;
        }
    };
    let answer = (|| {
        let token = env("SLATH_HOOK_TOKEN").ok_or("missing SLATH_HOOK_TOKEN")?;
        let socket = PathBuf::from(env("SLATH_IPC").ok_or("missing SLATH_IPC")?);
        let request = Request::ProcReceive {
            token,
            updates: updates.clone(),
        };
        ipc::call(&socket, &request, HOOK_TIMEOUT).map_err(|e| e.to_string())
    })();
    let results: Vec<ProcResult> = match answer {
        Ok(resp) => {
            for m in &resp.messages {
                let _ = writeln!(stderr, "{m}");
            }
            // Every command gets an answer, in order; one the server left
            // out is refused.
            updates
                .iter()
                .map(|u| {
                    resp.results
                        .iter()
                        .flat_map(|r| r.iter())
                        .find(|r| r.name == u.name)
                        .cloned()
                        .unwrap_or_else(|| ProcResult::refuse(&u.name, "not handled"))
                })
                .collect()
        }
        Err(e) => {
            let _ = writeln!(stderr, "slath: push rejected: {e}");
            updates
                .iter()
                .map(|u| ProcResult::refuse(&u.name, "the Slath server didn't answer"))
                .collect()
        }
    };
    let written = (|| {
        for r in &results {
            for line in r.report() {
                write_pkt(&mut output, &line)?;
            }
        }
        output.write_all(b"0000")?;
        output.flush()
    })();
    if written.is_err() {
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}

pub fn run(kind: HookKind) -> ExitCode {
    let mut stderr = std::io::stderr();
    if matches!(kind, HookKind::ProcReceive) {
        return proc_receive(&mut stderr);
    }
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
            // proc-receive returned above.
            HookKind::PostReceive | HookKind::ProcReceive => {
                Request::PostReceive { token, updates }
            }
        };
        ipc::call(&socket, &request, HOOK_TIMEOUT).map_err(|e| e.to_string())
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
        (HookKind::PreReceive | HookKind::ProcReceive, Err(e)) => {
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
