// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! The `slath` binary: the server and its helper subcommands.
#![forbid(unsafe_code)]

mod admin;
mod backup;
mod check;
mod config;
mod doctor;
mod healthcheck;
mod hook;
mod output;
mod serve;
mod setup;
mod ssh;
mod upgrade;
mod upgrade_notes;

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

/// Internal parts of `slath setup`.
#[derive(Subcommand)]
enum SetupHelper {
    /// Creates the database role and database as the `postgres` OS user
    /// (started by `slath setup`; reads its request on stdin).
    #[command(hide = true)]
    DbHelper,
}

#[derive(Subcommand)]
enum Command {
    /// Run the HTTP server, the IPC endpoint, and workers. A pending update
    /// is applied first (preflight, backup, migrations); if it can't be,
    /// the server refuses to start and changes nothing.
    Serve {
        /// Configuration file (default: $SLATH_CONFIG or /etc/slath/slath.toml).
        #[arg(long)]
        config: Option<PathBuf>,
    },
    /// Upgrade the database and data directory to this version: preflight,
    /// automatic backup, migrations, verification. `--check` only reports.
    /// Diagnose this installation: configuration, the SSH boundary, database
    /// and schema, Git, disk, backups, TLS, and the update preflight. Each
    /// problem has a stable code and a fix. Changes nothing.
    Doctor {
        #[arg(long)]
        config: Option<PathBuf>,
        #[arg(long, value_enum, default_value_t)]
        output: OutputFormat,
    },
    /// Set up this host (systemd): users, files, database, unit, sshd,
    /// schema, first administrator. Idempotent; `--check` changes nothing.
    Setup {
        #[command(subcommand)]
        helper: Option<SetupHelper>,
        /// List the changes without making them.
        #[arg(long)]
        check: bool,
        /// The URL people use to reach Slath (asked for if missing).
        #[arg(long)]
        public_url: Option<String>,
        /// Address the HTTP server listens on.
        #[arg(long, default_value = "127.0.0.1:3000")]
        listen: String,
        /// SSH port shown in clone URLs.
        #[arg(long, default_value_t = 22)]
        ssh_port: u16,
        /// The first site administrator's user name (asked for if missing).
        #[arg(long)]
        admin_username: Option<String>,
        #[arg(long)]
        admin_email: Option<String>,
        /// A PostgreSQL superuser connection for creating the role and
        /// database (default: the local socket as the `postgres` user).
        #[arg(long)]
        pg_admin_url: Option<String>,
        /// The installed binary to run (default: this one).
        #[arg(long)]
        binary: Option<PathBuf>,
        #[arg(long, value_enum, default_value_t)]
        output: OutputFormat,
    },
    Upgrade {
        #[arg(long)]
        config: Option<PathBuf>,
        /// Only run the preflight and show the plan; change nothing.
        #[arg(long)]
        check: bool,
        /// Skip the automatic pre-upgrade backup (not recommended).
        #[arg(long)]
        no_backup: bool,
        /// Where to write the backup (default: <data_dir>/backups).
        #[arg(long)]
        backup_dir: Option<PathBuf>,
        /// Don't ask for confirmation.
        #[arg(long)]
        yes: bool,
        /// Confirm that a required manual action from the upgrade notes is
        /// done (repeat for several).
        #[arg(long = "accept-action")]
        accept_actions: Vec<String>,
        #[arg(long, value_enum, default_value_t)]
        output: OutputFormat,
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
    /// Container health check: exit 0 when the local server is ready
    /// (`/-/readyz`), or with --ssh when sshd answers. Reads no
    /// configuration; for Docker, Podman, and Quadlet health checks.
    Healthcheck {
        /// Server address (default: SLATH__SERVER__LISTEN, else 127.0.0.1:3000).
        #[arg(long)]
        addr: Option<String>,
        /// Check an SSH server at this address instead, e.g. 127.0.0.1:22.
        #[arg(long, conflicts_with = "addr")]
        ssh: Option<String>,
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
    /// Back up the database and repositories, or restore a backup.
    Backup {
        #[command(subcommand)]
        command: BackupCommand,
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
enum BackupCommand {
    /// Write a backup into an empty directory while the server keeps running.
    Create {
        #[arg(long)]
        config: Option<PathBuf>,
        /// Target directory (created if missing; must be empty).
        #[arg(long)]
        output: PathBuf,
        /// Keep passwords and other secrets in the copied configuration.
        /// The master key is never included.
        #[arg(long)]
        include_secrets: bool,
        #[arg(long, value_enum, default_value_t)]
        output_format: OutputFormat,
    },
    /// Restore a backup into an empty database and data directory.
    Restore {
        #[arg(long)]
        config: Option<PathBuf>,
        #[arg(long)]
        input: PathBuf,
        #[arg(long, value_enum, default_value_t)]
        output_format: OutputFormat,
    },
}

#[derive(Subcommand)]
enum ConfigCommand {
    /// Rewrite a configuration file's deprecated keys to their
    /// replacements, keeping comments (the old file is kept as FILE.bak).
    Migrate {
        file: PathBuf,
        #[arg(long, value_enum, default_value_t)]
        output: OutputFormat,
    },
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
    /// Print a new master key (store it with mode 0600), or write it to a
    /// new file with --output.
    GenKey {
        /// Write the key to this file (mode 0600); fails if it exists.
        #[arg(long)]
        output: Option<PathBuf>,
    },
    /// Remove a user's second factors (lost device) and sign them out.
    /// Audited; the user is notified by email.
    ResetMfa {
        #[arg(long)]
        config: Option<PathBuf>,
        #[arg(long)]
        username: String,
        #[arg(long, value_enum, default_value_t)]
        output: OutputFormat,
    },
    /// Send the test message now (checks the [smtp] settings).
    SendTestEmail {
        #[arg(long)]
        config: Option<PathBuf>,
        /// Recipient.
        #[arg(long)]
        to: String,
        #[arg(long, value_enum, default_value_t)]
        output: OutputFormat,
    },
    /// Stop or re-enable a bot.
    Bot {
        #[command(subcommand)]
        command: BotCommand,
    },
    /// Inspect and control background migrations.
    BackgroundMigrations {
        #[command(subcommand)]
        command: BgCommand,
    },
    /// Re-encrypt secrets stored in the database with the current master
    /// key. Put the previous key file into secrets.old_key_files first.
    /// Safe to interrupt and run again.
    RotateKey {
        #[arg(long)]
        config: Option<PathBuf>,
        /// Rows per batch.
        #[arg(long, default_value_t = 100)]
        batch: i64,
        #[arg(long, value_enum, default_value_t)]
        output: OutputFormat,
    },
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
    /// Apply a bootstrap file: create the users, organizations, bots,
    /// repositories, and tokens it lists that don't exist yet. Running it
    /// again changes nothing.
    Apply {
        /// The bootstrap file (TOML); paths in it are relative to it.
        file: PathBuf,
        #[arg(long)]
        config: Option<PathBuf>,
        #[arg(long, value_enum, default_value_t)]
        output: OutputFormat,
    },
    /// Create a bot account for a user or organization, and optionally a
    /// token for it (printed once).
    CreateBot {
        #[arg(long)]
        config: Option<PathBuf>,
        /// The user or organization that owns the bot.
        #[arg(long)]
        owner: String,
        /// The bot's username.
        #[arg(long)]
        name: String,
        #[arg(long)]
        display_name: Option<String>,
        /// Also create a token with these scopes (comma-separated).
        #[arg(long, value_delimiter = ',')]
        token_scopes: Vec<String>,
        /// A repository (`namespace/name`) the token is restricted to;
        /// repeat for several. Bot tokens need at least one.
        #[arg(long = "repo")]
        repos: Vec<String>,
        /// Days until the token expires (default and maximum from the
        /// [auth] settings).
        #[arg(long)]
        token_days: Option<u32>,
        #[arg(long, value_enum, default_value_t)]
        output: OutputFormat,
    },
}

#[derive(Subcommand)]
pub enum BotCommand {
    /// Stop a bot at once: suspend it, revoke its tokens, remove its SSH
    /// keys, and refuse its pushes.
    Stop {
        name: String,
        #[arg(long)]
        config: Option<PathBuf>,
        #[arg(long, value_enum, default_value_t)]
        output: OutputFormat,
    },
    /// Re-enable a stopped bot. It has no credentials afterwards; its
    /// owner creates new ones.
    Start {
        name: String,
        #[arg(long)]
        config: Option<PathBuf>,
        #[arg(long, value_enum, default_value_t)]
        output: OutputFormat,
    },
    /// Freeze every bot of the instance, or of one organization: their
    /// credentials are refused until unfrozen, and nothing is revoked.
    Freeze {
        /// Only the bots this organization owns.
        #[arg(long)]
        org: Option<String>,
        #[arg(long)]
        config: Option<PathBuf>,
        #[arg(long, value_enum, default_value_t)]
        output: OutputFormat,
    },
    /// End a freeze; the bots' credentials work again.
    Unfreeze {
        #[arg(long)]
        org: Option<String>,
        #[arg(long)]
        config: Option<PathBuf>,
        #[arg(long, value_enum, default_value_t)]
        output: OutputFormat,
    },
}

#[derive(Subcommand)]
pub enum BgCommand {
    /// Show every background migration with progress and ETA.
    List {
        #[arg(long)]
        config: Option<PathBuf>,
        #[arg(long, value_enum, default_value_t)]
        output: OutputFormat,
    },
    /// Stop a migration after its current batch.
    Pause {
        name: String,
        #[arg(long)]
        config: Option<PathBuf>,
        #[arg(long, value_enum, default_value_t)]
        output: OutputFormat,
    },
    /// Continue a paused migration.
    Resume {
        name: String,
        #[arg(long)]
        config: Option<PathBuf>,
        #[arg(long, value_enum, default_value_t)]
        output: OutputFormat,
    },
    /// Run a failed migration again from where it stopped.
    Retry {
        name: String,
        #[arg(long)]
        config: Option<PathBuf>,
        #[arg(long, value_enum, default_value_t)]
        output: OutputFormat,
    },
    /// Run pending migrations to the end in the foreground (the server
    /// does this by itself; this is for maintenance windows).
    Run {
        #[arg(long)]
        config: Option<PathBuf>,
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
        Command::Serve { config } => serve::run(&config::resolve_path(config)),
        Command::Doctor { config, output } => doctor::run(&config::resolve_path(config), output),
        Command::Setup {
            helper: Some(SetupHelper::DbHelper),
            ..
        } => setup::db_helper(),
        Command::Setup {
            helper: None,
            check,
            public_url,
            listen,
            ssh_port,
            admin_username,
            admin_email,
            pg_admin_url,
            binary,
            output,
        } => setup::run(setup::Opts {
            check,
            public_url,
            listen,
            ssh_port,
            admin_username,
            admin_email,
            pg_admin_url,
            binary,
            output,
        }),
        Command::Upgrade {
            config,
            check,
            no_backup,
            backup_dir,
            yes,
            accept_actions,
            output,
        } => upgrade::run(
            &config::resolve_path(config),
            &upgrade::Opts {
                check,
                no_backup,
                backup_dir,
                yes,
                accept_actions,
                output,
            },
        ),
        Command::Migrate {
            config,
            dry_run,
            output,
        } => check::migrate(&config::resolve_path(config), dry_run, output),
        Command::Config {
            command: ConfigCommand::Check { config, output },
        } => check::config_check(&config::resolve_path(config), output),
        Command::Config {
            command: ConfigCommand::Migrate { file, output },
        } => check::config_migrate(&file, output),
        Command::Version { json, config } => check::version(json, config),
        Command::Healthcheck { addr, ssh } => healthcheck::run(addr.as_deref(), ssh.as_deref()),
        Command::Admin {
            command: AdminCommand::GenKey { output },
        } => check::gen_key(output.as_deref()),
        Command::Admin {
            command:
                AdminCommand::ResetMfa {
                    config,
                    username,
                    output,
                },
        } => admin::reset_mfa(&config::resolve_path(config), &username, output),
        Command::Admin {
            command: AdminCommand::SendTestEmail { config, to, output },
        } => admin::send_test_email(&config::resolve_path(config), &to, output),
        Command::Admin {
            command: AdminCommand::BackgroundMigrations { command },
        } => admin::background_migrations(command),
        Command::Admin {
            command: AdminCommand::Bot { command },
        } => admin::bot(command),
        Command::Admin {
            command:
                AdminCommand::RotateKey {
                    config,
                    batch,
                    output,
                },
        } => admin::rotate_key(&config::resolve_path(config), batch, output),
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
        Command::Admin {
            command:
                AdminCommand::Apply {
                    file,
                    config,
                    output,
                },
        } => admin::apply(&config::resolve_path(config), &file, output),
        Command::Admin {
            command:
                AdminCommand::CreateBot {
                    config,
                    owner,
                    name,
                    display_name,
                    token_scopes,
                    repos,
                    token_days,
                    output,
                },
        } => admin::create_bot(
            &config::resolve_path(config),
            admin::CreateBot {
                owner,
                name,
                display_name,
                token_scopes,
                repos,
                token_days,
            },
            output,
        ),
        Command::Backup {
            command:
                BackupCommand::Create {
                    config,
                    output,
                    include_secrets,
                    output_format,
                },
        } => backup::create(
            &config::resolve_path(config),
            &output,
            include_secrets,
            output_format,
        ),
        Command::Backup {
            command:
                BackupCommand::Restore {
                    config,
                    input,
                    output_format,
                },
        } => backup::restore(&config::resolve_path(config), &input, output_format),
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
