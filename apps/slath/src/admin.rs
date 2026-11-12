// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! `slath admin …`: bootstrap and recovery commands. They act as the
//! system, run non-interactively, and print JSON with `--output json`.

use std::io::Read;
use std::path::Path;
use std::process::ExitCode;
use std::sync::Arc;

use serde_json::json;
use slath_core::Secret;
use slath_core::authz::Subject;
use slath_service::{ServiceError, Services};

use crate::config;
use crate::output::{OutputFormat, fail, print_json, runtime};

/// Opens the database and builds the services for one command.
pub async fn services(path: &Path) -> Result<Services, String> {
    let svc = services_unchecked(path).await?;
    slath_db::check_schema(&svc.pool)
        .await
        .map_err(|e| e.to_string())?;
    Ok(svc)
}

/// Like [`services`] without the schema check (for `upgrade`, which
/// migrates with them).
pub async fn services_unchecked(path: &Path) -> Result<Services, String> {
    let cfg = config::load(path)?;
    let url = config::database_url(&cfg)?;
    let mut pc = slath_db::PoolConfig::new(url);
    pc.max_connections = 2;
    // A one-shot command can wait for a busy database.
    pc.acquire_timeout = std::time::Duration::from_secs(30);
    let pool = slath_db::connect(&pc).await.map_err(|e| e.to_string())?;
    let (key, old) = config::load_keys(&cfg)?;
    let home = cfg.storage.data_dir.join("git-home");
    std::fs::create_dir_all(&home).map_err(|e| format!("cannot create {}: {e}", home.display()))?;
    let runner = slath_git::Runner::new(slath_git::RunnerConfig::new(cfg.git.binary.clone(), home))
        .map_err(|e| e.to_string())?;
    Ok(Services::new(pool, Arc::new(cfg.cfg), &key, runner).with_old_keys(&key, &old))
}

/// A readable description of a service error for the terminal.
pub fn describe(e: &ServiceError) -> String {
    match e {
        ServiceError::Invalid(errors) => errors
            .iter()
            .map(|f| format!("{}: {}", f.field, f.message))
            .collect::<Vec<_>>()
            .join("; "),
        other => other.to_string(),
    }
}

/// Generates a random password: 24 characters from an unambiguous alphabet.
fn random_password() -> Result<String, String> {
    const ALPHABET: &[u8] = b"abcdefghijkmnopqrstuvwxyzABCDEFGHJKLMNPQRSTUVWXYZ23456789";
    let bytes = slath_auth::token::random_bytes::<24>().map_err(|e| e.to_string())?;
    Ok(bytes
        .iter()
        .map(|b| ALPHABET[usize::from(*b) % ALPHABET.len()] as char)
        .collect())
}

pub struct CreateUser {
    pub username: String,
    pub email: Option<String>,
    pub display_name: Option<String>,
    pub admin: bool,
    pub password_stdin: bool,
}

pub fn create_user(path: &Path, args: CreateUser, format: OutputFormat) -> ExitCode {
    let (password, generated) = if args.password_stdin {
        let mut s = String::new();
        if let Err(e) = std::io::stdin().read_to_string(&mut s) {
            return fail(format, &format!("cannot read the password from stdin: {e}"));
        }
        (s.trim_end_matches(['\r', '\n']).to_owned(), false)
    } else {
        match random_password() {
            Ok(p) => (p, true),
            Err(e) => return fail(format, &e),
        }
    };
    let rt = match runtime() {
        Ok(rt) => rt,
        Err(e) => return fail(format, &e),
    };
    let result = rt.block_on(async {
        let svc = services(path).await?;
        svc.create_account(
            &Subject::System,
            slath_service::accounts::NewAccount {
                username: args.username.clone(),
                email: args.email.clone(),
                display_name: args.display_name.clone(),
                password: Some(Secret::new(password.clone())),
                is_admin: args.admin,
            },
        )
        .await
        .map_err(|e| describe(&e))
    });
    match result {
        Ok(id) => {
            match format {
                OutputFormat::Text => {
                    println!(
                        "created user {} ({id}){}",
                        args.username,
                        if args.admin {
                            " as site administrator"
                        } else {
                            ""
                        }
                    );
                    if generated {
                        println!("password: {password}");
                        println!("Store it now; it can't be shown again.");
                    }
                }
                OutputFormat::Json => print_json(&json!({
                    "ok": true,
                    "user": {"id": id.to_string(), "username": args.username, "admin": args.admin},
                    "password": if generated { Some(password) } else { None },
                })),
            }
            ExitCode::SUCCESS
        }
        Err(e) => fail(format, &e),
    }
}

pub struct CreateBot {
    pub owner: String,
    pub name: String,
    pub display_name: Option<String>,
    pub token_scopes: Vec<String>,
    pub repos: Vec<String>,
    pub token_days: Option<u32>,
}

/// `slath admin create-bot`: creates a bot for a user or organization and,
/// with `--token-scopes`, a token printed once.
pub fn create_bot(path: &Path, args: CreateBot, format: OutputFormat) -> ExitCode {
    let rt = match runtime() {
        Ok(rt) => rt,
        Err(e) => return fail(format, &e),
    };
    let result = rt.block_on(async {
        let svc = services(path).await?;
        let token = (!args.token_scopes.is_empty()).then(|| slath_service::bots::NewBotToken {
            name: "created by slath admin create-bot".to_owned(),
            scopes: args.token_scopes.clone(),
            repos: args.repos.clone(),
            expires_in_days: args.token_days,
        });
        if token.is_none() && !args.repos.is_empty() {
            return Err("--repo needs --token-scopes".to_owned());
        }
        svc.create_bot_as_system(
            &args.owner,
            &args.name,
            args.display_name.as_deref().unwrap_or(""),
            token,
        )
        .await
        .map_err(|e| describe(&e))
    });
    match result {
        Ok((bot, token)) => {
            match format {
                OutputFormat::Text => {
                    println!(
                        "created bot {} ({}) owned by {}",
                        bot.name, bot.id, bot.owner_name
                    );
                    if let Some(t) = &token {
                        println!("token: {}", t.expose());
                        println!("Store it now; it can't be shown again.");
                    }
                }
                OutputFormat::Json => print_json(&json!({
                    "ok": true,
                    "bot": {"id": bot.id.to_string(), "username": bot.name, "owner": bot.owner_name},
                    "token": token.as_ref().map(|t| t.expose().clone()),
                })),
            }
            ExitCode::SUCCESS
        }
        Err(e) => fail(format, &e),
    }
}

/// Writes a secret to `path` with mode 0600 (directories 0700), replacing
/// what was there.
fn write_secret(path: &Path, value: &str) -> Result<(), String> {
    use std::io::Write;
    use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)
            .map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    }
    // Replace by rename, so a reader never sees a half-written file.
    let tmp = path.with_extension("tmp-slath");
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&tmp)
        .map_err(|e| format!("cannot write {}: {e}", tmp.display()))?;
    f.write_all(value.as_bytes())
        .and_then(|()| f.write_all(b"\n"))
        .and_then(|()| f.sync_all())
        .map_err(|e| format!("cannot write {}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, path).map_err(|e| format!("cannot write {}: {e}", path.display()))
}

/// `slath admin apply FILE`: applies a bootstrap file. Paths in it are
/// relative to the file. Generated passwords and new tokens go to their
/// files (mode 0600); a generated password without a file is printed once.
pub fn apply(path: &Path, file: &Path, format: OutputFormat) -> ExitCode {
    let text = match std::fs::read_to_string(file) {
        Ok(t) => t,
        Err(e) => return fail(format, &format!("cannot read {}: {e}", file.display())),
    };
    let mut plan: slath_service::apply::Plan = match toml::from_str(&text) {
        Ok(p) => p,
        Err(e) => return fail(format, &format!("{}: {e}", file.display())),
    };
    let base = file.parent().unwrap_or_else(|| Path::new("."));
    let resolve = |p: &str| base.join(p);
    for u in &mut plan.users {
        if let Some(f) = &u.password_file
            && let Ok(pw) = std::fs::read_to_string(resolve(f))
        {
            u.password = Some(Secret::new(pw.trim_end_matches(['\r', '\n']).to_owned()));
        }
    }
    for t in &mut plan.tokens {
        t.file_present = resolve(&t.file).is_file();
    }
    let rt = match runtime() {
        Ok(rt) => rt,
        Err(e) => return fail(format, &e),
    };
    let result = rt.block_on(async {
        let svc = services(path).await?;
        svc.apply(plan).await.map_err(|e| describe(&e))
    });
    let changes = match result {
        Ok(c) => c,
        Err(e) => return fail(format, &e),
    };
    let mut out = Vec::new();
    for c in &changes {
        let mut printed = None;
        if let Some(secret) = &c.secret {
            match &c.file {
                Some(f) => {
                    if let Err(e) = write_secret(&resolve(f), secret.expose()) {
                        return fail(format, &e);
                    }
                }
                None => printed = Some(secret.expose().clone()),
            }
        }
        out.push((c, printed));
    }
    match format {
        OutputFormat::Text => {
            for (c, printed) in &out {
                let file = c
                    .file
                    .as_deref()
                    .map_or(String::new(), |f| format!(" (written to {f})"));
                println!("{} {} {}{file}", c.action, c.kind.replace('_', " "), c.name);
                if let Some(p) = printed {
                    println!("  password: {p}  (store it now; it can't be shown again)");
                }
            }
            println!(
                "{} change{}",
                changes.len(),
                if changes.len() == 1 { "" } else { "s" }
            );
        }
        OutputFormat::Json => print_json(&json!({
            "ok": true,
            "changes": out.iter().map(|(c, printed)| json!({
                "kind": c.kind,
                "name": c.name,
                "action": c.action,
                "file": c.file,
                "password": printed,
            })).collect::<Vec<_>>(),
        })),
    }
    ExitCode::SUCCESS
}

/// `slath fsck [--reconcile] [--repo ID]`.
pub fn fsck(path: &Path, repo: Option<String>, reconcile: bool, format: OutputFormat) -> ExitCode {
    let rt = match runtime() {
        Ok(rt) => rt,
        Err(e) => return fail(format, &e),
    };
    let result = rt.block_on(async {
        let svc = services(path).await?;
        let only = match repo {
            Some(r) => Some(
                r.parse()
                    .map_err(|_| format!("--repo: {r:?} is not a repository ID"))?,
            ),
            None => None,
        };
        svc.fsck(only, reconcile).await.map_err(|e| describe(&e))
    });
    let checks = match result {
        Ok(c) => c,
        Err(e) => return fail(format, &e),
    };
    let clean = checks
        .iter()
        .all(|c| c.is_clean() || (c.repaired && c.missing_revisions.is_empty()));
    match format {
        OutputFormat::Text => {
            for c in &checks {
                if c.orphan {
                    println!("orphan   {} (a directory without a repository)", c.name);
                } else if c.missing_on_disk {
                    println!("missing  {} (no data on disk)", c.name);
                } else if !c.ref_differences.is_empty() {
                    let verb = if c.repaired { "repaired" } else { "differs " };
                    println!("{verb} {} ({} refs)", c.name, c.ref_differences.len());
                    for u in &c.ref_differences {
                        println!(
                            "         {} {} -> {}",
                            String::from_utf8_lossy(&u.name),
                            &u.old[..12],
                            &u.new[..12]
                        );
                    }
                }
                if c.unpinned_revisions > 0 {
                    let verb = if c.repaired { "repinned" } else { "unpinned" };
                    println!(
                        "{verb} {} ({} merge request revisions)",
                        c.name, c.unpinned_revisions
                    );
                }
                for (mr, seq) in &c.missing_revisions {
                    println!(
                        "lost     {} (merge request {mr}, revision {seq}: commits are gone)",
                        c.name
                    );
                }
            }
            println!(
                "{} repositories checked, {}",
                checks.iter().filter(|c| !c.orphan).count(),
                if clean { "clean" } else { "problems found" }
            );
        }
        OutputFormat::Json => print_json(&json!({
            "ok": clean,
            "repositories": checks.iter().map(|c| json!({
                "id": c.repo_id.map(|i| i.to_string()),
                "name": c.name,
                "orphan": c.orphan,
                "missing_on_disk": c.missing_on_disk,
                "repaired": c.repaired,
                "unpinned_revisions": c.unpinned_revisions,
                "missing_revisions": c.missing_revisions.iter().map(|(mr, seq)| json!({
                    "merge_request": mr.to_string(),
                    "revision": seq,
                })).collect::<Vec<_>>(),
                "ref_differences": c.ref_differences.iter().map(|u| json!({
                    "ref": String::from_utf8_lossy(&u.name),
                    "old": u.old,
                    "new": u.new,
                })).collect::<Vec<_>>(),
            })).collect::<Vec<_>>(),
        })),
    }
    if clean {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

/// `slath admin rotate-key`: re-encrypts every secret with the current key
/// and retires keys nothing uses any more.
pub fn rotate_key(path: &Path, batch: i64, format: OutputFormat) -> ExitCode {
    let rt = match runtime() {
        Ok(rt) => rt,
        Err(e) => return fail(format, &e),
    };
    let result = rt.block_on(async {
        let svc = services(path).await?;
        svc.rotate_keys(batch).await.map_err(|e| describe(&e))
    });
    match result {
        Ok(report) => {
            let ok = report.failed.is_empty();
            match format {
                OutputFormat::Text => {
                    println!(
                        "re-encrypted {} value(s) with master key {}",
                        report.reencrypted, report.current_key
                    );
                    for k in &report.retired {
                        println!(
                            "retired key {k}; its file can be removed from secrets.old_key_files"
                        );
                    }
                    for f in &report.failed {
                        eprintln!("slath: {} row {}: {}", f.table, f.row, f.error);
                    }
                    if !ok {
                        eprintln!(
                            "slath: {} value(s) were not re-encrypted; add the missing key files to \
                             secrets.old_key_files and run this command again",
                            report.failed.len()
                        );
                    }
                }
                OutputFormat::Json => print_json(&json!({"ok": ok, "rotation": report})),
            }
            if ok {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            }
        }
        Err(e) => fail(format, &e),
    }
}

fn bg_json(s: &slath_db::bgmigrations::Status) -> serde_json::Value {
    let now = time::OffsetDateTime::now_utc();
    json!({
        "name": s.name,
        "introduced_in": s.introduced_in,
        "state": s.state.as_str(),
        "processed": s.processed,
        "total_estimate": s.total_estimate,
        "percent": s.percent(),
        "eta_seconds": s.eta(now).map(|d| d.as_secs()),
        "last_error": s.last_error,
    })
}

/// `slath admin background-migrations …`.
pub fn background_migrations(cmd: crate::BgCommand) -> ExitCode {
    use crate::BgCommand as B;
    use slath_db::bgmigrations::Control;
    let (config, format) = match &cmd {
        B::List { config, output }
        | B::Run { config, output }
        | B::Pause { config, output, .. }
        | B::Resume { config, output, .. }
        | B::Retry { config, output, .. } => (config.clone(), *output),
    };
    let path = config::resolve_path(config);
    let rt = match runtime() {
        Ok(rt) => rt,
        Err(e) => return fail(format, &e),
    };
    let result: Result<Vec<slath_db::bgmigrations::Status>, String> = rt.block_on(async {
        let svc = services(&path).await?;
        svc.register_background_migrations()
            .await
            .map_err(|e| describe(&e))?;
        let control = match &cmd {
            B::Pause { name, .. } => Some((name, Control::Pause)),
            B::Resume { name, .. } => Some((name, Control::Resume)),
            B::Retry { name, .. } => Some((name, Control::Retry)),
            B::List { .. } | B::Run { .. } => None,
        };
        if let Some((name, c)) = control {
            svc.control_background_migration(&Subject::System, name, c)
                .await
                .map_err(|e| match e {
                    ServiceError::NotFound => format!("no background migration named {name}"),
                    other => describe(&other),
                })?;
        }
        if matches!(cmd, B::Run { .. }) {
            svc.run_background_migrations(&|| false)
                .await
                .map_err(|e| describe(&e))?;
        }
        svc.background_migration_status(&Subject::System)
            .await
            .map_err(|e| describe(&e))
    });
    match result {
        Ok(list) => {
            match format {
                OutputFormat::Text if list.is_empty() => println!("no background migrations"),
                OutputFormat::Text => {
                    let now = time::OffsetDateTime::now_utc();
                    for s in &list {
                        let progress = match (s.percent(), s.total_estimate) {
                            (Some(p), Some(t)) => format!("{p}% ({}/{t})", s.processed),
                            _ => format!("{} processed", s.processed),
                        };
                        let eta = s
                            .eta(now)
                            .map(|d| format!(", about {} s left", d.as_secs()))
                            .unwrap_or_default();
                        println!("{:<32} {:<8} {progress}{eta}", s.name, s.state.as_str());
                        if let Some(e) = &s.last_error {
                            println!("    error: {e}");
                        }
                    }
                }
                OutputFormat::Json => print_json(&json!({
                    "ok": true,
                    "background_migrations": list.iter().map(bg_json).collect::<Vec<_>>(),
                })),
            }
            if list
                .iter()
                .any(|s| s.state == slath_db::bgmigrations::State::Failed)
            {
                ExitCode::FAILURE
            } else {
                ExitCode::SUCCESS
            }
        }
        Err(e) => fail(format, &e),
    }
}

/// `slath admin send-test-email --to ADDR`: sends synchronously and reports
/// the SMTP server's answer.
pub fn send_test_email(path: &Path, to: &str, format: OutputFormat) -> ExitCode {
    let rt = match runtime() {
        Ok(rt) => rt,
        Err(e) => return fail(format, &e),
    };
    let result = rt.block_on(async {
        let svc = services(path).await?;
        if !svc.email_enabled() {
            return Err("email is not configured (set [smtp] host)".to_owned());
        }
        let email = svc.test_email(to).map_err(|e| describe(&e))?;
        let id = uuid::Uuid::now_v7().to_string();
        svc.deliver(&email, &id).await.map_err(|e| e.to_string())
    });
    match result {
        Ok(()) => {
            match format {
                OutputFormat::Text => println!("test message sent to {to}"),
                OutputFormat::Json => print_json(&json!({"ok": true, "to": to})),
            }
            ExitCode::SUCCESS
        }
        Err(e) => fail(format, &e),
    }
}

/// `slath admin reset-mfa --username NAME`.
pub fn reset_mfa(path: &Path, username: &str, format: OutputFormat) -> ExitCode {
    let rt = match runtime() {
        Ok(rt) => rt,
        Err(e) => return fail(format, &e),
    };
    let result = rt.block_on(async {
        let svc = services(path).await?;
        svc.reset_mfa(&Subject::System, username)
            .await
            .map_err(|e| match e {
                ServiceError::NotFound => format!("no user named {username}"),
                other => describe(&other),
            })
    });
    match result {
        Ok(()) => {
            match format {
                OutputFormat::Text => println!(
                    "removed the second factors of {username} and signed them out; they can sign in with their password"
                ),
                OutputFormat::Json => print_json(&json!({"ok": true, "username": username})),
            }
            ExitCode::SUCCESS
        }
        Err(e) => fail(format, &e),
    }
}
