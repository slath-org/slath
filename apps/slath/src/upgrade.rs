// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! `slath upgrade [--check]` and the preflight of `serve --migrate`.
//!
//! An upgrade runs: preflight (nothing changes before every check passed)
//! → read-only maintenance of a running server → backup → schema migrations
//! one by one under a lock → hook and repository config refresh (a
//! background migration) → post-flight checks → a record in
//! `upgrade_history`. Every step is safe to repeat, so an interrupted
//! upgrade is finished by running the command again.

use std::io::{BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use serde::Serialize;
use serde_json::json;
use sqlx::PgPool;
use sqlx::pool::PoolConnection;

use crate::config::{self, Loaded};
use crate::output::{OutputFormat, fail, print_json, runtime};
use crate::upgrade_notes::{self as notes, Notes, Version};

/// Advisory lock held by whoever migrates (`upgrade`, `serve --migrate`).
const UPGRADE_LOCK: i64 = 0x736c_6174_6875_7067; // "slathupg"
/// Oldest supported PostgreSQL major version.
const MIN_POSTGRES: i64 = 15;

pub struct Opts {
    pub check: bool,
    pub no_backup: bool,
    pub backup_dir: Option<PathBuf>,
    pub yes: bool,
    /// Required manual actions (by ID) the operator confirms are done.
    pub accept_actions: Vec<String>,
    pub output: OutputFormat,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
enum Mark {
    Ok,
    Warn,
    Fail,
}

#[derive(Debug, Clone, Serialize)]
struct Item {
    name: &'static str,
    mark: Mark,
    detail: String,
}

/// Everything the preflight found out. Gathering it changes nothing.
#[derive(Debug, Serialize)]
struct Preflight {
    from_version: Option<String>,
    from_schema: i32,
    to_version: String,
    to_schema: i32,
    pending: Vec<(i64, String)>,
    items: Vec<Item>,
    notes: Vec<String>,
    required_actions: Vec<(String, String)>,
    background_migrations: Vec<String>,
    server: Option<slath_git::ipc::ServerStatus>,
    server_running: bool,
    /// An earlier run stopped before finishing; this one completes it.
    interrupted: bool,
}

impl Preflight {
    fn ok(&self) -> bool {
        self.items.iter().all(|i| i.mark != Mark::Fail)
    }

    fn push(&mut self, name: &'static str, mark: Mark, detail: impl Into<String>) {
        self.items.push(Item {
            name,
            mark,
            detail: detail.into(),
        });
    }
}

/// Simulated crash for tests: `SLATH_UPGRADE_FAULT=<step>` aborts the
/// process at that step, like a kill would.
fn fault_point(step: &str) {
    if std::env::var("SLATH_UPGRADE_FAULT").is_ok_and(|s| s == step) {
        eprintln!("slath: simulated crash at {step}");
        std::process::abort();
    }
}

async fn connect(cfg: &Loaded) -> Result<PgPool, String> {
    let mut pc = slath_db::PoolConfig::new(config::database_url(cfg)?);
    pc.max_connections = 4;
    pc.acquire_timeout = Duration::from_secs(30);
    slath_db::connect(&pc).await.map_err(|e| e.to_string())
}

async fn postgres_major(pool: &PgPool) -> Result<i64, String> {
    let n: String = sqlx::query_scalar("SHOW server_version_num")
        .fetch_one(pool)
        .await
        .map_err(|e| e.to_string())?;
    Ok(n.parse::<i64>().unwrap_or(0) / 10_000)
}

async fn table_exists(pool: &PgPool, table: &str) -> bool {
    sqlx::query_scalar::<_, bool>("SELECT to_regclass($1) IS NOT NULL")
        .bind(format!("public.{table}"))
        .fetch_one(pool)
        .await
        .unwrap_or(false)
}

/// The unfinished upgrade recorded by an earlier run that stopped, if any.
async fn unfinished(pool: &PgPool) -> Option<serde_json::Value> {
    if !table_exists(pool, "instance_settings").await {
        return None;
    }
    slath_db::settings::get(pool, slath_db::settings::UPGRADE_IN_PROGRESS)
        .await
        .ok()
        .flatten()
}

/// The release the database comes from: the source of an unfinished
/// upgrade, the last upgrade's target, the last version that served it, or
/// the oldest release with its schema.
async fn from_version(pool: &PgPool, all: &[Notes], schema: i32) -> Option<Version> {
    if let Some(v) = unfinished(pool)
        .await
        .and_then(|u| u["from_version"].as_str().and_then(|v| v.parse().ok()))
    {
        return Some(v);
    }
    if table_exists(pool, "upgrade_history").await
        && let Ok(Some(v)) = sqlx::query_scalar::<_, String>(
            "SELECT to_version FROM upgrade_history WHERE outcome = 'success'
             ORDER BY started_at DESC LIMIT 1",
        )
        .fetch_optional(pool)
        .await
        && let Ok(v) = v.parse()
    {
        return Some(v);
    }
    if let Ok(Some(v)) = slath_db::settings::get(pool, slath_db::settings::SERVED_BY).await
        && let Some(v) = v.as_str().and_then(|s| s.parse().ok())
    {
        return Some(v);
    }
    notes::release_for_schema(all, schema).map(|n| n.version.clone())
}

/// Bytes used under `dir`, not following symlinks, skipping `skip`.
fn du(dir: &Path, skip: &Path) -> u64 {
    let mut total = 0;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else {
            continue;
        };
        for e in rd.flatten() {
            let p = e.path();
            if p == skip {
                continue;
            }
            match e.file_type() {
                Ok(t) if t.is_dir() => stack.push(p),
                Ok(t) if t.is_file() => total += e.metadata().map_or(0, |m| m.len()),
                _ => {}
            }
        }
    }
    total
}

/// Free bytes on the file system holding `path` (or its nearest existing parent).
fn free_bytes(path: &Path) -> Option<u64> {
    let existing = path.ancestors().find(|p| p.exists())?;
    let st = nix::sys::statvfs::statvfs(existing).ok()?;
    Some(st.blocks_available() as u64 * st.fragment_size() as u64)
}

fn human(b: u64) -> String {
    const GIB: f64 = 1024.0 * 1024.0 * 1024.0;
    const MIB: f64 = 1024.0 * 1024.0;
    let b = b as f64;
    if b >= GIB {
        format!("{:.1} GiB", b / GIB)
    } else {
        format!("{:.1} MiB", b / MIB)
    }
}

/// Asks a running server for its status. `Ok(None)`: none is running.
fn server_status(socket: &Path) -> Result<Option<slath_git::ipc::ServerStatus>, String> {
    if !socket.exists() {
        return Ok(None);
    }
    match slath_git::ipc::call(
        socket,
        &slath_git::ipc::Request::Status,
        Duration::from_secs(3),
    ) {
        Ok(r) => Ok(r.status.map(|b| *b).or(Some(slath_git::ipc::ServerStatus {
            version: "older than 0.2.0".to_owned(),
            schema: 0,
            maintenance: false,
        }))),
        Err(slath_git::ipc::IpcError::Io(e))
            if matches!(
                e.kind(),
                std::io::ErrorKind::ConnectionRefused | std::io::ErrorKind::NotFound
            ) =>
        {
            Ok(None)
        }
        // Something answers but can't speak the status request: an older server.
        Err(_) => Ok(Some(slath_git::ipc::ServerStatus {
            version: "older than 0.2.0".to_owned(),
            schema: 0,
            maintenance: false,
        })),
    }
}

/// Holds the upgrade lock on its own connection while it lives.
struct Lock(PoolConnection<sqlx::Postgres>);

async fn try_lock(pool: &PgPool) -> Result<Option<Lock>, String> {
    let mut c = pool.acquire().await.map_err(|e| e.to_string())?;
    let got: bool = sqlx::query_scalar("SELECT pg_try_advisory_lock($1)")
        .bind(UPGRADE_LOCK)
        .fetch_one(&mut *c)
        .await
        .map_err(|e| e.to_string())?;
    Ok(got.then_some(Lock(c)))
}

impl Lock {
    async fn release(mut self) {
        let _ = sqlx::query("SELECT pg_advisory_unlock($1)")
            .bind(UPGRADE_LOCK)
            .execute(&mut *self.0)
            .await;
    }
}

/// The checks shared by `upgrade` and `serve --migrate`. Takes the lock
/// and returns it if available.
async fn preflight(
    cfg: &Loaded,
    pool: &PgPool,
    opts_accept: &[String],
    backup_dir: Option<&Path>,
    with_backup: bool,
) -> Result<(Preflight, Option<Lock>), String> {
    let all = notes::all()?;
    let to = notes::current()?;
    let interrupted = unfinished(pool).await;
    // An interrupted upgrade reports where it started, not where it stopped.
    let from_schema = match interrupted
        .as_ref()
        .and_then(|u| u["from_schema"].as_i64())
        .and_then(|v| i32::try_from(v).ok())
    {
        Some(v) => v,
        None => slath_db::schema_version(pool)
            .await
            .map_err(|e| e.to_string())?
            .unwrap_or(0),
    };
    let from = if from_schema == 0 {
        None
    } else {
        from_version(pool, &all, from_schema).await
    };
    let pending = slath_db::pending_migrations(pool)
        .await
        .map_err(|e| e.to_string())?;
    let mut p = Preflight {
        from_version: from.as_ref().map(ToString::to_string),
        from_schema,
        to_version: to.to_string(),
        to_schema: slath_db::REQUIRED_SCHEMA,
        pending,
        items: Vec::new(),
        notes: Vec::new(),
        required_actions: Vec::new(),
        background_migrations: Vec::new(),
        server: None,
        server_running: false,
        interrupted: interrupted.is_some(),
    };
    if p.interrupted {
        p.push(
            "previous run",
            Mark::Warn,
            "an earlier upgrade stopped before finishing; this run completes it",
        );
    }

    // Versions.
    match postgres_major(pool).await {
        Ok(v) if v >= MIN_POSTGRES => p.push("postgres", Mark::Ok, format!("PostgreSQL {v}")),
        Ok(v) => p.push(
            "postgres",
            Mark::Fail,
            format!("PostgreSQL {v} is too old; {MIN_POSTGRES} or newer is required"),
        ),
        Err(e) => p.push("postgres", Mark::Fail, e),
    }
    // Extensions the migrations enable (PostgreSQL's contrib modules).
    let missing: Vec<String> = sqlx::query_scalar(
        "SELECT x FROM unnest(ARRAY['citext', 'pg_trgm']) x
          WHERE NOT EXISTS (SELECT 1 FROM pg_available_extensions WHERE name = x)",
    )
    .fetch_all(pool)
    .await
    .unwrap_or_default();
    if missing.is_empty() {
        p.push(
            "extensions",
            Mark::Ok,
            "citext, pg_trgm available".to_owned(),
        );
    } else {
        p.push(
            "extensions",
            Mark::Fail,
            format!(
                "PostgreSQL extension(s) {} not installed; install PostgreSQL's contrib modules",
                missing.join(", ")
            ),
        );
    }
    let home = {
        let h = cfg.storage.data_dir.join("git-home");
        if h.is_dir() { h } else { PathBuf::from("/") }
    };
    match crate::check::git_version(&cfg.git.binary, &home).await {
        Ok(v) => p.push("git", Mark::Ok, format!("Git {v}")),
        Err(e) => p.push("git", Mark::Fail, e),
    }

    // The jump itself.
    if from_schema > slath_db::REQUIRED_SCHEMA {
        p.push(
            "source",
            Mark::Fail,
            format!(
                "the database (schema {from_schema}) is newer than this binary ({}); downgrades \
                 aren't supported: restore the pre-upgrade backup with the version that made it",
                slath_db::REQUIRED_SCHEMA
            ),
        );
    } else if let Some(f) = &from {
        let min = all
            .iter()
            .find(|n| n.version == to)
            .map(|n| n.min_upgrade_from.clone());
        match min {
            Some(min) if *f < min => p.push(
                "source",
                Mark::Fail,
                format!("upgrading from v{f} directly isn't supported; first upgrade to v{min}"),
            ),
            _ => p.push(
                "source",
                Mark::Ok,
                format!(
                    "v{f} (schema {from_schema}) → v{to} (schema {})",
                    p.to_schema
                ),
            ),
        }
    } else if from_schema == 0 {
        p.push("source", Mark::Ok, "empty database: a new installation");
    } else {
        p.push(
            "source",
            Mark::Warn,
            format!("schema {from_schema} isn't from a known release; continuing by schema"),
        );
    }

    // Upgrade notes of the jump.
    let jump = from
        .as_ref()
        .map(|f| notes::between(&all, f, &to))
        .unwrap_or_default();
    p.notes = notes::render(&jump);
    p.background_migrations = jump
        .iter()
        .flat_map(|n| &n.background_migrations)
        .map(|b| format!("{} ({})", b.name, b.impact))
        .collect();
    for (v, a) in notes::required_actions(&jump) {
        p.required_actions.push((a.id.clone(), a.text.clone()));
        if opts_accept.contains(&a.id) {
            p.push(
                "action",
                Mark::Ok,
                format!("v{v}: {} (confirmed done)", a.text),
            );
        } else {
            p.push(
                "action",
                Mark::Fail,
                format!(
                    "v{v}: {} — do this first, then confirm with --accept-action {}",
                    a.text, a.id
                ),
            );
        }
    }

    // Configuration.
    if cfg.warnings.is_empty() {
        p.push("config", Mark::Ok, "valid");
    }
    for w in &cfg.warnings {
        p.push("config", Mark::Warn, w.clone());
    }

    // Data directory and disk space.
    match data_dir_probe(&cfg.storage.data_dir) {
        Ok(()) => p.push(
            "data_dir",
            Mark::Ok,
            format!("{} is writable", cfg.storage.data_dir.display()),
        ),
        Err(e) => p.push("data_dir", Mark::Fail, e),
    }
    if with_backup {
        let target =
            backup_dir.map_or_else(|| cfg.storage.data_dir.join("backups"), Path::to_path_buf);
        let db_size: i64 = sqlx::query_scalar("SELECT pg_database_size(current_database())")
            .fetch_one(pool)
            .await
            .unwrap_or(0);
        let need = u64::try_from(db_size).unwrap_or(0)
            + du(&cfg.storage.data_dir, &cfg.storage.data_dir.join("backups"));
        // Room for the backup plus a margin.
        let need = need + need / 10 + 64 * 1024 * 1024;
        match free_bytes(&target) {
            Some(free) if free >= need => p.push(
                "disk",
                Mark::Ok,
                format!(
                    "{} free at {} (backup needs about {})",
                    human(free),
                    target.display(),
                    human(need)
                ),
            ),
            Some(free) => p.push(
                "disk",
                Mark::Fail,
                format!(
                    "only {} free at {}, the backup needs about {}; free space, use --backup-dir, \
                     or skip the backup with --no-backup",
                    human(free),
                    target.display(),
                    human(need)
                ),
            ),
            None => p.push(
                "disk",
                Mark::Warn,
                format!("cannot measure free space at {}", target.display()),
            ),
        }
    }

    // Nobody else migrating.
    let lock = try_lock(pool).await?;
    if lock.is_some() {
        p.push("lock", Mark::Ok, "no other upgrade or migration is running");
    } else {
        p.push(
            "lock",
            Mark::Fail,
            "another `slath upgrade` or `serve --migrate` holds the upgrade lock",
        );
    }

    // Jobs (information only; they resume afterwards).
    if table_exists(pool, "jobs").await {
        let ready: i64 =
            sqlx::query_scalar("SELECT count(*) FROM jobs WHERE state IN ('ready', 'running')")
                .fetch_one(pool)
                .await
                .unwrap_or(0);
        p.push(
            "jobs",
            Mark::Ok,
            format!("{ready} queued job(s); they continue after the upgrade"),
        );
    }
    Ok((p, lock))
}

fn data_dir_probe(dir: &Path) -> Result<(), String> {
    if !dir.is_dir() {
        return Err(format!("{} doesn't exist", dir.display()));
    }
    let probe = dir.join(format!(".slath-upgrade-probe-{}", std::process::id()));
    std::fs::write(&probe, b"ok").map_err(|e| format!("{} is not writable: {e}", dir.display()))?;
    std::fs::remove_file(&probe).map_err(|e| format!("cannot remove {}: {e}", probe.display()))
}

fn print_preflight(p: &Preflight) {
    println!(
        "Current: {} (schema {}) → Target: v{} (schema {})",
        p.from_version
            .as_deref()
            .map_or_else(|| "unknown".to_owned(), |v| format!("v{v}")),
        p.from_schema,
        p.to_version,
        p.to_schema
    );
    println!("Preflight:");
    for i in &p.items {
        let m = match i.mark {
            Mark::Ok => "✓",
            Mark::Warn => "!",
            Mark::Fail => "✗",
        };
        println!("  {m} {:<9} {}", i.name, i.detail);
    }
    if !p.notes.is_empty() {
        println!("Upgrade notes:");
        for n in &p.notes {
            println!("  - {n}");
        }
    }
    println!(
        "Plan: {}{} schema migration(s){}",
        if p.server_running {
            "maintenance mode → "
        } else {
            ""
        },
        p.pending.len(),
        if p.background_migrations.is_empty() {
            String::new()
        } else {
            format!(
                " → {} background migration(s)",
                p.background_migrations.len()
            )
        }
    );
}

fn confirm(question: &str) -> bool {
    if !std::io::stdin().is_terminal() {
        return false;
    }
    print!("{question} [y/N] ");
    let _ = std::io::stdout().flush();
    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line).is_ok() && matches!(line.trim(), "y" | "Y" | "yes")
}

/// `slath upgrade`.
pub fn run(config_path: &Path, opts: &Opts) -> ExitCode {
    let format = opts.output;
    let cfg = match config::load(config_path) {
        Ok(c) => c,
        Err(e) => return fail(format, &format!("{e} (nothing was changed)")),
    };
    let rt = match runtime() {
        Ok(rt) => rt,
        Err(e) => return fail(format, &e),
    };
    match rt.block_on(upgrade(config_path, &cfg, opts)) {
        Ok(code) => code,
        Err(e) => fail(format, &e),
    }
}

async fn upgrade(config_path: &Path, cfg: &Loaded, opts: &Opts) -> Result<ExitCode, String> {
    let format = opts.output;
    let pool = connect(cfg).await?;
    let backup = !opts.no_backup;
    let (mut p, lock) = preflight(
        cfg,
        &pool,
        &opts.accept_actions,
        opts.backup_dir.as_deref(),
        backup && !opts.check,
    )
    .await?;
    let socket = cfg.storage.run_dir.join("slath.sock");
    match server_status(&socket)? {
        None => p.push("server", Mark::Ok, "no server is running"),
        Some(s) if s.version.starts_with("older") => {
            p.server_running = true;
            p.push(
                "server",
                Mark::Fail,
                format!(
                    "a server {} is running; stop it before upgrading",
                    s.version
                ),
            );
            p.server = Some(s);
        }
        Some(s) => {
            p.server_running = true;
            p.push(
                "server",
                Mark::Ok,
                format!(
                    "v{} is running; it becomes read-only during the upgrade",
                    s.version
                ),
            );
            p.server = Some(s);
        }
    }
    if opts.no_backup && !opts.check {
        p.push(
            "backup",
            Mark::Warn,
            "--no-backup: there will be no way back except your own backups",
        );
    }
    let nothing_to_do = !p.interrupted
        && p.pending.is_empty()
        && p.from_version.as_deref() == Some(p.to_version.as_str());
    let report = |p: &Preflight| match format {
        OutputFormat::Text => print_preflight(p),
        OutputFormat::Json => {
            print_json(&json!({"ok": p.ok(), "check": opts.check, "preflight": p}))
        }
    };
    if opts.check {
        if let Some(l) = lock {
            l.release().await;
        }
        report(&p);
        return Ok(if p.ok() {
            ExitCode::SUCCESS
        } else {
            ExitCode::FAILURE
        });
    }
    if format == OutputFormat::Text {
        print_preflight(&p);
    }
    let Some(lock) = lock.filter(|_| p.ok()) else {
        if format == OutputFormat::Json {
            report(&p);
        }
        return Err("preflight failed; nothing was changed".to_owned());
    };
    if nothing_to_do {
        lock.release().await;
        if format == OutputFormat::Text {
            println!("Already up to date.");
        } else {
            print_json(&json!({"ok": true, "up_to_date": true, "preflight": p}));
        }
        return Ok(ExitCode::SUCCESS);
    }
    if !opts.yes && !confirm("Proceed with the upgrade?") {
        lock.release().await;
        return Err("aborted (pass --yes to run without asking); nothing was changed".to_owned());
    }
    let started = time::OffsetDateTime::now_utc();
    // Set as soon as the backup exists, so a later failure still names it.
    let mut backup_path = None;
    let result = execute(config_path, cfg, &pool, &p, opts, started, &mut backup_path).await;
    let outcome = match &result {
        Ok(()) => "success",
        Err(_) => "failed",
    };
    record_history(
        &pool,
        &p,
        backup_path.as_deref(),
        started,
        outcome,
        result.as_ref().err(),
    )
    .await;
    if result.is_ok()
        && let Err(e) =
            slath_db::settings::delete(&pool, slath_db::settings::UPGRADE_IN_PROGRESS).await
    {
        eprintln!("slath: cannot clear the upgrade marker: {e}");
    }
    lock.release().await;
    match result {
        Ok(()) => {
            match format {
                OutputFormat::Text => {
                    println!("Upgrade to v{} finished.", p.to_version);
                    if let Some(b) = &backup_path {
                        println!("Pre-upgrade backup: {}", b.display());
                    }
                    if p.server_running {
                        println!(
                            "Restart the server with this version now; it stays read-only until then."
                        );
                    }
                }
                OutputFormat::Json => print_json(&json!({
                    "ok": true,
                    "from_version": p.from_version,
                    "to_version": p.to_version,
                    "backup": backup_path.map(|b| b.display().to_string()),
                    "restart_required": p.server_running,
                })),
            }
            Ok(ExitCode::SUCCESS)
        }
        Err(e) => {
            let back = backup_path.map_or_else(
                || "No backup was taken.".to_owned(),
                |b| {
                    format!(
                        "To go back instead, restore the backup at {} with v{}.",
                        b.display(),
                        p.from_version.as_deref().unwrap_or("the previous version")
                    )
                },
            );
            Err(format!(
                "{e}\nRun `slath upgrade` again to continue where it stopped (every step can be repeated). {back}"
            ))
        }
    }
}

/// The changing part of the upgrade. Stores the backup's path in
/// `backup_out` as soon as it is complete.
async fn execute(
    config_path: &Path,
    cfg: &Loaded,
    pool: &PgPool,
    p: &Preflight,
    opts: &Opts,
    started: time::OffsetDateTime,
    backup_out: &mut Option<PathBuf>,
) -> Result<(), String> {
    let text = opts.output == OutputFormat::Text;
    let svc = crate::admin::services_unchecked(config_path).await?;
    // 1. Read-only maintenance of a running server.
    if p.server_running {
        svc.enter_maintenance(
            &format!("upgrade to v{}", p.to_version),
            Some(&p.to_version),
            false,
        )
        .await
        .map_err(|e| e.to_string())?;
        let socket = cfg.storage.run_dir.join("slath.sock");
        let mut acknowledged = false;
        for _ in 0..30 {
            if server_status(&socket)?.is_some_and(|s| s.maintenance) {
                acknowledged = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
        if !acknowledged {
            return Err("the running server didn't enter maintenance mode in 15 s".to_owned());
        }
        if text {
            println!("maintenance mode on");
        }
    }
    fault_point("after-maintenance");
    // 2. Backup.
    if !opts.no_backup {
        let stamp = started.unix_timestamp();
        let from = p.from_version.as_deref().unwrap_or("unknown");
        let dir = opts
            .backup_dir
            .clone()
            .unwrap_or_else(|| cfg.storage.data_dir.join("backups"))
            .join(format!("pre-upgrade-{from}-{}-{stamp}", p.to_version));
        if text {
            println!("backing up to {} …", dir.display());
        }
        crate::backup::create_with(config_path, &dir, false, true).await?;
        *backup_out = Some(dir);
    }
    fault_point("after-backup");
    // Until the upgrade succeeds, a later run knows it has to finish it, even
    // once the schema is current. Recorded after the backup, so restoring
    // the backup doesn't bring it back.
    if !p.interrupted && table_exists(pool, "instance_settings").await {
        let marker = json!({
            "from_version": p.from_version,
            "from_schema": p.from_schema,
            "to_version": p.to_version,
        });
        slath_db::settings::set(pool, slath_db::settings::UPGRADE_IN_PROGRESS, &marker)
            .await
            .map_err(|e| format!("cannot record the upgrade's start: {e}"))?;
    }
    // 3. Schema migrations, one at a time.
    for (i, (version, desc)) in p.pending.iter().enumerate() {
        let t = std::time::Instant::now();
        slath_db::MIGRATOR
            .run_to(*version, pool)
            .await
            .map_err(|e| format!("migration {version:04} ({desc}) failed: {e}"))?;
        if text {
            println!(
                "applied migration {version:04} {desc} ({} ms)",
                t.elapsed().as_millis()
            );
        }
        if i == 0 {
            fault_point("after-first-migration");
        }
    }
    fault_point("after-migrations");
    // 4. Hooks and repository config.
    crate::serve::prepare_dirs(cfg)?;
    let new = svc
        .register_background_migrations()
        .await
        .map_err(|e| e.to_string())?;
    if text && !new.is_empty() {
        println!("background migrations registered: {}", new.join(", "));
    }
    fault_point("after-hooks");
    // 5. Post-flight.
    slath_db::check_schema(pool)
        .await
        .map_err(|e| e.to_string())?;
    let ids = slath_db::repos::all_ids(pool)
        .await
        .map_err(|e| e.to_string())?;
    let mut problems = Vec::new();
    for id in ids.iter().take(10) {
        match svc.fsck(Some(*id), false).await {
            Ok(checks) => {
                for c in checks.iter().filter(|c| c.missing_on_disk) {
                    problems.push(format!("repository {} has no data on disk", c.name));
                }
            }
            Err(e) => problems.push(format!("repository {id}: {e}")),
        }
    }
    let users: i64 = sqlx::query_scalar("SELECT count(*) FROM users")
        .fetch_one(pool)
        .await
        .map_err(|e| format!("smoke query failed: {e}"))?;
    config::load(config_path)?;
    if !problems.is_empty() {
        return Err(format!(
            "post-flight checks failed: {}",
            problems.join("; ")
        ));
    }
    if text {
        println!(
            "post-flight: schema {} current, {} repositories sampled, {users} users",
            slath_db::REQUIRED_SCHEMA,
            ids.len().min(10)
        );
    }
    // 6. Leave maintenance to the restarted server.
    if p.server_running {
        svc.enter_maintenance(
            &format!(
                "upgrade to v{} finished; waiting for the server to restart",
                p.to_version
            ),
            Some(&p.to_version),
            true,
        )
        .await
        .map_err(|e| e.to_string())?;
    }
    Ok(())
}

async fn record_history(
    pool: &PgPool,
    p: &Preflight,
    backup: Option<&Path>,
    started: time::OffsetDateTime,
    outcome: &str,
    error: Option<&String>,
) {
    if !table_exists(pool, "upgrade_history").await {
        return;
    }
    let preflight = serde_json::to_value(&p.items).unwrap_or_default();
    let r = sqlx::query(
        "INSERT INTO upgrade_history (id, from_version, to_version, from_schema, to_schema,
             backup_path, preflight, started_at, finished_at, outcome, error)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, now(), $9, $10)",
    )
    .bind(uuid::Uuid::now_v7())
    .bind(&p.from_version)
    .bind(&p.to_version)
    .bind(p.from_schema)
    .bind(p.to_schema)
    .bind(backup.map(|b| b.display().to_string()))
    .bind(preflight)
    .bind(started)
    .bind(outcome)
    .bind(error.map(|e| e.chars().take(2000).collect::<String>()))
    .execute(pool)
    .await;
    if let Err(e) = r {
        eprintln!("slath: cannot record the upgrade in upgrade_history: {e}");
    }
}

/// `serve --migrate` (the container entrypoint): the same preflight; it
/// refuses to migrate when a check fails or the jump needs a manual action.
/// Takes a backup first only with `upgrade.auto_backup`.
pub async fn serve_migrate(config_path: &Path, cfg: &Loaded, pool: &PgPool) -> Result<(), String> {
    let pending = slath_db::pending_migrations(pool)
        .await
        .map_err(|e| e.to_string())?;
    if pending.is_empty() {
        return Ok(());
    }
    let backup = cfg.upgrade.auto_backup;
    let (p, lock) = preflight(cfg, pool, &[], None, backup).await?;
    let failed: Vec<String> = p
        .items
        .iter()
        .filter(|i| i.mark == Mark::Fail)
        .map(|i| format!("{}: {}", i.name, i.detail))
        .collect();
    let Some(lock) = lock.filter(|_| failed.is_empty()) else {
        return Err(format!(
            "refusing to migrate the database automatically: {}. Run `slath upgrade` \
             (https://slath.org/docs/admin/upgrading/)",
            failed.join("; ")
        ));
    };
    for w in p.items.iter().filter(|i| i.mark == Mark::Warn) {
        tracing::warn!(check = w.name, "{}", w.detail);
    }
    let started = time::OffsetDateTime::now_utc();
    let mut backup_path = None;
    if backup {
        let dir = cfg.storage.data_dir.join("backups").join(format!(
            "pre-upgrade-{}-{}-{}",
            p.from_version.as_deref().unwrap_or("unknown"),
            p.to_version,
            started.unix_timestamp()
        ));
        crate::backup::create_with(config_path, &dir, false, true).await?;
        tracing::info!(path = %dir.display(), "pre-upgrade backup written");
        backup_path = Some(dir);
    }
    let result = slath_db::MIGRATOR
        .run(pool)
        .await
        .map_err(|e| format!("migration failed: {e}"));
    if result.is_ok() {
        tracing::info!(count = p.pending.len(), "migrations applied");
    }
    let outcome = if result.is_ok() { "success" } else { "failed" };
    record_history(
        pool,
        &p,
        backup_path.as_deref(),
        started,
        outcome,
        result.as_ref().err(),
    )
    .await;
    lock.release().await;
    result
}
