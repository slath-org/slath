// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! `slath config check`, `slath version`, `slath migrate`, `slath admin gen-key`.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use serde_json::json;
use slath_auth::MasterKey;
use slath_core::config::Config;
use slath_git::{Runner, RunnerConfig};

use crate::config;
use crate::output::{OutputFormat, fail, print_json, runtime};

/// One line of the check report.
struct Item {
    name: &'static str,
    ok: bool,
    detail: String,
}

/// Runs `git --version` with the configured binary and checks the minimum.
pub async fn git_version(binary: &Path, home: &Path) -> Result<String, String> {
    let runner = Runner::new(RunnerConfig::new(binary.to_path_buf(), home.to_path_buf()))
        .map_err(|e| e.to_string())?;
    runner
        .check_version()
        .await
        .map(|v| v.to_string())
        .map_err(|e| e.to_string())
}

/// A directory to run Git in for checks: `<data_dir>/git-home` if present.
fn check_home(cfg: &Config) -> PathBuf {
    let home = cfg.storage.data_dir.join("git-home");
    if home.is_dir() {
        home
    } else {
        PathBuf::from("/")
    }
}

fn data_dir_writable(dir: &Path) -> Result<String, String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    let probe = dir.join(format!(".slath-write-test-{}", std::process::id()));
    std::fs::write(&probe, b"ok").map_err(|e| format!("{} is not writable: {e}", dir.display()))?;
    let _ = std::fs::remove_file(&probe);
    Ok(format!("{} is writable", dir.display()))
}

async fn database(cfg: &Config) -> Result<String, String> {
    let url = config::database_url(cfg)?;
    let mut pc = slath_db::PoolConfig::new(url);
    pc.max_connections = 1;
    let pool = slath_db::connect(&pc).await.map_err(|e| e.to_string())?;
    let version = slath_db::schema_version(&pool)
        .await
        .map_err(|e| e.to_string())?
        .unwrap_or(0);
    let required = slath_db::REQUIRED_SCHEMA;
    Ok(match version.cmp(&required) {
        std::cmp::Ordering::Equal => format!("connected; schema {version} is current"),
        std::cmp::Ordering::Less => {
            format!("connected; schema {version}, {required} required (run `slath migrate`)")
        }
        std::cmp::Ordering::Greater => {
            return Err(format!(
                "schema {version} is newer than this binary supports ({required})"
            ));
        }
    })
}

pub fn config_check(path: &Path, format: OutputFormat) -> ExitCode {
    let cfg = match config::load(path) {
        Ok(c) => c,
        Err(e) => return fail(format, &e),
    };
    let rt = match runtime() {
        Ok(rt) => rt,
        Err(e) => return fail(format, &e),
    };
    let mut items = vec![Item {
        name: "config",
        ok: true,
        detail: format!("{} is valid", path.display()),
    }];
    let mut push = |name, r: Result<String, String>| {
        let (ok, detail) = match r {
            Ok(d) => (true, d),
            Err(d) => (false, d),
        };
        items.push(Item { name, ok, detail });
    };
    push(
        "git",
        rt.block_on(git_version(&cfg.git.binary, &check_home(&cfg)))
            .map(|v| format!("git {v} at {}", cfg.git.binary.display())),
    );
    push("data_dir", data_dir_writable(&cfg.storage.data_dir));
    push("database", rt.block_on(database(&cfg)));
    if let Some(key) = &cfg.secrets.master_key_file {
        push(
            "master_key",
            MasterKey::load(key)
                .map(|k| format!("{} (key id {})", key.display(), k.id()))
                .map_err(|e| e.to_string()),
        );
    }
    let all_ok = items.iter().all(|i| i.ok);
    match format {
        OutputFormat::Text => {
            for i in &items {
                let mark = if i.ok { "ok  " } else { "FAIL" };
                println!("{mark} {:<11} {}", i.name, i.detail);
            }
        }
        OutputFormat::Json => print_json(&json!({
            "ok": all_ok,
            "checks": items.iter().map(|i| json!({"name": i.name, "ok": i.ok, "detail": i.detail})).collect::<Vec<_>>(),
        })),
    }
    if all_ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

pub fn version(as_json: bool, config_path: Option<PathBuf>) -> ExitCode {
    let version = env!("CARGO_PKG_VERSION");
    let schema = slath_db::REQUIRED_SCHEMA;
    let binary = config_path
        .or_else(|| std::env::var_os("SLATH_CONFIG").map(PathBuf::from))
        .and_then(|p| config::load(&p).ok())
        .map_or_else(|| PathBuf::from("/usr/bin/git"), |c| c.git.binary.clone());
    let git = runtime()
        .ok()
        .and_then(|rt| rt.block_on(git_version(&binary, Path::new("/"))).ok());
    if as_json {
        print_json(&json!({"version": version, "schema": schema, "git": git}));
    } else {
        println!("slath {version} (schema {schema})");
        match git {
            Some(g) => println!("git {g}"),
            None => println!("git: not found or unsupported at {}", binary.display()),
        }
    }
    ExitCode::SUCCESS
}

pub fn migrate(path: &Path, dry_run: bool, format: OutputFormat) -> ExitCode {
    let result = (|| {
        let cfg = config::load(path)?;
        let url = config::database_url(&cfg)?;
        let rt = runtime()?;
        rt.block_on(async {
            let mut pc = slath_db::PoolConfig::new(url);
            pc.max_connections = 2;
            let pool = slath_db::connect(&pc).await.map_err(|e| e.to_string())?;
            let pending = slath_db::pending_migrations(&pool)
                .await
                .map_err(|e| e.to_string())?;
            if !dry_run {
                slath_db::migrate(&pool).await.map_err(|e| e.to_string())?;
            }
            let schema = slath_db::schema_version(&pool)
                .await
                .map_err(|e| e.to_string())?
                .unwrap_or(0);
            Ok::<_, String>((pending, schema))
        })
    })();
    match result {
        Ok((pending, schema)) => {
            match format {
                OutputFormat::Text => {
                    let verb = if dry_run { "pending" } else { "applied" };
                    for (v, d) in &pending {
                        println!("{verb} {v:04} {d}");
                    }
                    println!("schema version {schema}");
                }
                OutputFormat::Json => print_json(&json!({
                    "ok": true,
                    "dry_run": dry_run,
                    "migrations": pending.iter().map(|(v, d)| json!({"version": v, "description": d})).collect::<Vec<_>>(),
                    "schema": schema,
                })),
            }
            ExitCode::SUCCESS
        }
        Err(e) => fail(format, &e),
    }
}

pub fn gen_key() -> ExitCode {
    match MasterKey::generate() {
        Ok(k) => {
            print!("{}", k.encode());
            ExitCode::SUCCESS
        }
        Err(e) => fail(OutputFormat::Text, &e.to_string()),
    }
}
