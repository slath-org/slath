// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! `slath config check`, `slath version`, `slath migrate`, `slath admin gen-key`.

use std::os::unix::fs::MetadataExt;
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

/// Checks that members of `storage.socket_group` (the SSH login) can't
/// reach the server's private files: the data directory, the master key,
/// and the database URL file.
fn socket_group(cfg: &Config) -> Result<String, String> {
    let Some(gid) = config::socket_gid(cfg)? else {
        return Ok(
            "storage.socket_group is not set; only the server's user can connect".to_owned(),
        );
    };
    let mut private = vec![cfg.storage.data_dir.clone()];
    private.extend(cfg.secrets.master_key_file.iter().cloned());
    private.extend(cfg.secrets.old_key_files.iter().cloned());
    private.extend(cfg.database.url_file.iter().cloned());
    let exposed: Vec<String> = private
        .iter()
        .filter(|p| reachable_by_group(p, gid))
        .map(|p| p.display().to_string())
        .collect();
    if exposed.is_empty() {
        Ok(format!(
            "group {gid} can connect to the socket but not read the server's files"
        ))
    } else {
        Err(format!(
            "readable by group {gid} or by everyone: {}",
            exposed.join(", ")
        ))
    }
}

/// True if a member of group `gid` who doesn't own anything on the way could
/// read `path`: every directory above it searchable and the path itself
/// readable (or, for a directory, searchable). Missing paths aren't.
fn reachable_by_group(path: &Path, gid: u32) -> bool {
    let Ok(path) = std::fs::canonicalize(path) else {
        return false;
    };
    let allows = |p: &Path, bits: u32| {
        std::fs::metadata(p).is_ok_and(|m| group_may(m.mode(), m.gid(), gid, bits))
    };
    let parents_searchable = path.ancestors().skip(1).all(|dir| allows(dir, 0o1));
    let target_bits = if path.is_dir() { 0o5 } else { 0o4 };
    parents_searchable && allows(&path, target_bits)
}

/// True if a non-owner in group `gid` has any of `bits` (rwx, 0o7) on a file
/// with `mode` and group `file_gid`: the group bits if it's the file's group,
/// the bits for everyone else otherwise.
fn group_may(mode: u32, file_gid: u32, gid: u32, bits: u32) -> bool {
    let shift = if file_gid == gid { 3 } else { 0 };
    (mode >> shift) & bits != 0
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

/// The checks of `slath config check` after loading: `(name, ok, detail)`.
pub fn run_checks(cfg: &config::Loaded) -> Result<Vec<(&'static str, bool, String)>, String> {
    let rt = runtime()?;
    let mut out = Vec::new();
    let mut push = |name, r: Result<String, String>| match r {
        Ok(d) => out.push((name, true, d)),
        Err(d) => out.push((name, false, d)),
    };
    push(
        "git",
        rt.block_on(git_version(&cfg.git.binary, &check_home(cfg)))
            .map(|v| format!("git {v} at {}", cfg.git.binary.display())),
    );
    push("data_dir", data_dir_writable(&cfg.storage.data_dir));
    push("socket", socket_group(cfg));
    push("database", rt.block_on(database(cfg)));
    if let Some(key) = &cfg.secrets.master_key_file {
        push(
            "master_key",
            MasterKey::load(key)
                .map(|k| format!("{} (key id {})", key.display(), k.id()))
                .map_err(|e| e.to_string()),
        );
    }
    for old in &cfg.secrets.old_key_files {
        push(
            "old_key",
            MasterKey::load(old)
                .map(|k| format!("{} (key id {}, for rotation)", old.display(), k.id()))
                .map_err(|e| e.to_string()),
        );
    }
    Ok(out)
}

pub fn config_check(path: &Path, format: OutputFormat) -> ExitCode {
    let cfg = match config::load(path) {
        Ok(c) => c,
        Err(e) => return fail(format, &e),
    };
    let checks = match run_checks(&cfg) {
        Ok(c) => c,
        Err(e) => return fail(format, &e),
    };
    let mut items = vec![Item {
        name: "config",
        ok: true,
        detail: format!("{} is valid", path.display()),
    }];
    items.extend(
        checks
            .into_iter()
            .map(|(name, ok, detail)| Item { name, ok, detail }),
    );
    let all_ok = items.iter().all(|i| i.ok);
    match format {
        OutputFormat::Text => {
            for i in &items {
                let mark = if i.ok { "ok  " } else { "FAIL" };
                println!("{mark} {:<11} {}", i.name, i.detail);
            }
            for w in &cfg.warnings {
                println!("warn config      {w}");
            }
        }
        OutputFormat::Json => print_json(&json!({
            "ok": all_ok,
            "checks": items.iter().map(|i| json!({"name": i.name, "ok": i.ok, "detail": i.detail})).collect::<Vec<_>>(),
            "warnings": cfg.warnings,
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
            pc.acquire_timeout = std::time::Duration::from_secs(30);
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

pub fn gen_key(output: Option<&Path>) -> ExitCode {
    let Some(path) = output else {
        return match MasterKey::generate() {
            Ok(k) => {
                print!("{}", k.encode());
                ExitCode::SUCCESS
            }
            Err(e) => fail(OutputFormat::Text, &e.to_string()),
        };
    };
    if path.exists() {
        return fail(
            OutputFormat::Text,
            &format!(
                "{} already exists; refusing to replace a master key",
                path.display()
            ),
        );
    }
    match MasterKey::load_or_create(path) {
        Ok(_) => {
            eprintln!("master key written to {}", path.display());
            ExitCode::SUCCESS
        }
        Err(e) => fail(OutputFormat::Text, &e.to_string()),
    }
}

/// `slath config migrate FILE`: rewrites deprecated keys (backup in `FILE.bak`).
pub fn config_migrate(path: &Path, format: OutputFormat) -> ExitCode {
    match config::migrate_file(path) {
        Ok(changes) => {
            match format {
                OutputFormat::Text if changes.is_empty() => {
                    println!("{} uses no deprecated keys", path.display());
                }
                OutputFormat::Text => {
                    for c in &changes {
                        println!("{c}");
                    }
                    println!("previous version saved as {}.bak", path.display());
                }
                OutputFormat::Json => print_json(&json!({"ok": true, "changes": changes})),
            }
            ExitCode::SUCCESS
        }
        Err(e) => fail(format, &e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn chmod(p: &Path, mode: u32) {
        std::fs::set_permissions(p, std::fs::Permissions::from_mode(mode)).unwrap();
    }

    #[test]
    fn group_and_other_bits() {
        // The file's own group: group bits.
        assert!(group_may(0o640, 7, 7, 0o4));
        assert!(!group_may(0o600, 7, 7, 0o4));
        assert!(
            !group_may(0o604, 7, 7, 0o4),
            "other bits don't apply to the group"
        );
        // Another group: the bits for everyone else.
        assert!(!group_may(0o640, 7, 8, 0o4));
        assert!(group_may(0o604, 7, 8, 0o4));
        assert!(group_may(0o711, 7, 8, 0o1));
    }

    #[test]
    fn private_files_behind_a_closed_directory_are_unreachable() {
        let dir = tempfile::tempdir().unwrap();
        let gid = std::fs::metadata(dir.path()).unwrap().gid();
        let key = dir.path().join("master.key");
        std::fs::write(&key, "k").unwrap();
        chmod(&key, 0o644);
        chmod(dir.path(), 0o700);
        assert!(!reachable_by_group(&key, gid));
        assert!(!reachable_by_group(dir.path(), gid));
        chmod(dir.path(), 0o750);
        chmod(&key, 0o600);
        assert!(!reachable_by_group(&key, gid));
        assert!(!reachable_by_group(&dir.path().join("missing"), gid));
        // Only if the temp directory's ancestors are searchable for its group
        // (they may not be in a sandbox): the open key is then reachable.
        chmod(&key, 0o640);
        let above_open =
            dir.path().ancestors().skip(1).all(|d| {
                std::fs::metadata(d).is_ok_and(|m| group_may(m.mode(), m.gid(), gid, 0o1))
            });
        assert_eq!(reachable_by_group(&key, gid), above_open);
    }
}
