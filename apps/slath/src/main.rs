// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! The `slath` binary: the server and its helper subcommands.
#![forbid(unsafe_code)]

mod admin;
mod check;
mod config;
mod hook;
mod output;
mod serve;
mod ssh;

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};

use crate::output::OutputFormat;

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
        /// Configuration file (default: $SLATH_CONFIG or /etc/slath/slath.toml).
        #[arg(long)]
        config: Option<PathBuf>,
        /// Apply pending database migrations before starting.
        #[arg(long)]
        migrate: bool,
    },
    /// Apply pending database migrations.
    Migrate {
        #[arg(long)]
        config: Option<PathBuf>,
        /// Only report pending migrations.
        #[arg(long)]
        dry_run: bool,
        #[arg(long, value_enum, default_value_t)]
        output: OutputFormat,
    },
    /// Configuration commands.
    Config {
        #[command(subcommand)]
        command: ConfigCommand,
    },
    /// Print version information.
    Version {
        /// Print JSON instead of text.
        #[arg(long)]
        json: bool,
        /// Configuration file, used to find the Git binary.
        #[arg(long)]
        config: Option<PathBuf>,
    },
    /// Check repositories against the database; with --reconcile, record
    /// refs changed outside Slath.
    Fsck {
        #[arg(long)]
        config: Option<PathBuf>,
        /// Record differences (as pushes with transport "reconcile").
        #[arg(long)]
        reconcile: bool,
        /// Check only this repository (ID).
        #[arg(long)]
        repo: Option<String>,
        #[arg(long, value_enum, default_value_t)]
        output: OutputFormat,
    },
    /// Administration commands.
    Admin {
        #[command(subcommand)]
        command: AdminCommand,
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

#[derive(Subcommand)]
enum ConfigCommand {
    /// Validate the configuration and the environment (Git, directories,
    /// database, master key).
    Check {
        #[arg(long)]
        config: Option<PathBuf>,
        #[arg(long, value_enum, default_value_t)]
        output: OutputFormat,
    },
}

#[derive(Subcommand)]
enum AdminCommand {
    /// Print a new master key (store it with mode 0600).
    GenKey,
    /// Create a user account (bootstrap and recovery). Without
    /// --password-stdin, a random password is generated and printed once.
    CreateUser {
        #[arg(long)]
        config: Option<PathBuf>,
        #[arg(long)]
        username: String,
        #[arg(long)]
        email: Option<String>,
        #[arg(long)]
        display_name: Option<String>,
        /// Make the user a site administrator.
        #[arg(long)]
        admin: bool,
        /// Read the password from standard input.
        #[arg(long)]
        password_stdin: bool,
        #[arg(long, value_enum, default_value_t)]
        output: OutputFormat,
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
        Command::Serve { config, migrate } => serve::run(&config::resolve_path(config), migrate),
        Command::Migrate {
            config,
            dry_run,
            output,
        } => check::migrate(&config::resolve_path(config), dry_run, output),
        Command::Config {
            command: ConfigCommand::Check { config, output },
        } => check::config_check(&config::resolve_path(config), output),
        Command::Version { json, config } => check::version(json, config),
        Command::Admin {
            command: AdminCommand::GenKey,
        } => check::gen_key(),
        Command::Admin {
            command:
                AdminCommand::CreateUser {
                    config,
                    username,
                    email,
                    display_name,
                    admin,
                    password_stdin,
                    output,
                },
        } => admin::create_user(
            &config::resolve_path(config),
            admin::CreateUser {
                username,
                email,
                display_name,
                admin,
                password_stdin,
            },
            output,
        ),
        Command::Fsck {
            config,
            reconcile,
            repo,
            output,
        } => admin::fsck(&config::resolve_path(config), repo, reconcile, output),
        Command::Hook { hook } => hook::run(hook),
        Command::SshKeys {
            socket,
            fingerprint,
        } => ssh::keys(&socket, &fingerprint),
        Command::SshServe { socket, key_id } => ssh::serve(&socket, &key_id),
    }
}
