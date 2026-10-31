// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! The `slath` binary: the server and its helper subcommands.
#![forbid(unsafe_code)]

mod config;
mod hook;
mod serve;
mod ssh;

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "slath", version, about = "Self-hosted Git and Jujutsu forge")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Run the HTTP server, the IPC endpoint, and workers.
    Serve {
        /// Configuration file.
        #[arg(long, default_value = "/etc/slath/slath.toml")]
        config: PathBuf,
    },
    /// Git hook entry points (invoked by Git only).
    Hook {
        #[command(subcommand)]
        hook: hook::HookKind,
    },
    /// sshd `AuthorizedKeysCommand`: print the authorized_keys line for a key.
    SshKeys {
        /// IPC socket of `slath serve`.
        #[arg(long, default_value = ssh::DEFAULT_SOCKET)]
        socket: PathBuf,
        /// SHA-256 key fingerprint (sshd's `%f`).
        fingerprint: String,
    },
    /// sshd forced command: serve one Git transport request.
    SshServe {
        /// IPC socket of `slath serve`.
        #[arg(long, default_value = ssh::DEFAULT_SOCKET)]
        socket: PathBuf,
        #[arg(long)]
        key_id: String,
    },
}

fn main() -> ExitCode {
    // Git runs the managed hooks through symlinks named after the hook, so
    // `pre-receive`/`post-receive` as argv[0] means "run that hook". This
    // avoids shell shims entirely.
    if let Some(kind) = std::env::args_os()
        .next()
        .and_then(|a| hook::HookKind::from_argv0(&a))
    {
        return hook::run(kind);
    }
    let cli = Cli::parse();
    match cli.command {
        Command::Serve { config } => serve::run(&config),
        Command::Hook { hook } => hook::run(hook),
        Command::SshKeys {
            socket,
            fingerprint,
        } => ssh::keys(&socket, &fingerprint),
        Command::SshServe { socket, key_id } => ssh::serve(&socket, &key_id),
    }
}
