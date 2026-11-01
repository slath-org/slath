// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! CLI smoke tests. Having an integration test here also makes Cargo build
//! the `slath` binary that the suites in `tests/` launch.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::process::Command;

#[test]
fn version_flag_works() {
    let out = Command::new(env!("CARGO_BIN_EXE_slath"))
        .arg("--version")
        .output()
        .unwrap();
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stdout).starts_with("slath "));
}

#[test]
fn serve_rejects_unknown_config_keys() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = dir.path().join("slath.toml");
    std::fs::write(&cfg, "[server]\nbogus = 1\n").unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_slath"))
        .args(["serve", "--config"])
        .arg(&cfg)
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("bogus"));
}

#[test]
fn hook_without_server_fails_closed() {
    let out = Command::new(env!("CARGO_BIN_EXE_slath"))
        .args(["hook", "pre-receive"])
        .env("SLATH_HOOK_TOKEN", "x")
        .env("SLATH_IPC", "/nonexistent/slath.sock")
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("push rejected"));
}

fn slath(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_slath"))
        .args(args)
        .output()
        .unwrap()
}

fn write_config(dir: &std::path::Path, extra: &str) -> std::path::PathBuf {
    let cfg = dir.join("slath.toml");
    let db = std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgres://slath:slath@127.0.0.1:54329/slath".to_owned());
    let git = std::env::var("SLATH_TEST_GIT").unwrap_or_else(|_| "/usr/bin/git".to_owned());
    std::fs::write(
        &cfg,
        format!(
            "[server]\npublic_url = \"http://localhost:3000\"\n\
             [storage]\ndata_dir = \"{d}/data\"\nrun_dir = \"{d}/run\"\n\
             [database]\nurl = \"{db}\"\n[git]\nbinary = \"{git}\"\n{extra}",
            d = dir.display()
        ),
    )
    .unwrap();
    cfg
}

#[test]
fn version_json_reports_schema_and_git() {
    let out = slath(&["version", "--json"]);
    assert!(out.status.success());
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["version"], env!("CARGO_PKG_VERSION"));
    assert!(v["schema"].as_i64().unwrap() >= 1);
    assert!(v.get("git").is_some());
}

#[test]
fn config_check_reports_each_item() {
    let dir = tempfile::tempdir().unwrap();
    let key = dir.path().join("master.key");
    let gen_key = slath(&["admin", "gen-key"]);
    assert!(gen_key.status.success());
    std::fs::write(&key, &gen_key.stdout).unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o600)).unwrap();
    let cfg = write_config(
        dir.path(),
        &format!("[secrets]\nmaster_key_file = \"{}\"\n", key.display()),
    );
    let out = slath(&[
        "config",
        "check",
        "--output",
        "json",
        "--config",
        cfg.to_str().unwrap(),
    ]);
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let names: Vec<&str> = v["checks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        [
            "config",
            "git",
            "data_dir",
            "socket",
            "database",
            "master_key"
        ]
    );
    assert_eq!(v["ok"], true, "{v}");
    assert!(out.status.success());

    // A key readable by others fails the check and names the problem.
    std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o644)).unwrap();
    let out = slath(&["config", "check", "--config", cfg.to_str().unwrap()]);
    assert!(!out.status.success());
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("FAIL master_key"), "{text}");

    // With a socket group, the master key must be closed to it: a key the
    // group can read fails the check, even behind group-searchable dirs.
    use std::os::unix::fs::MetadataExt;
    let gid = std::fs::metadata(&key).unwrap().gid().to_string();
    let check = || {
        let out = Command::new(env!("CARGO_BIN_EXE_slath"))
            .args(["config", "check", "--output", "json", "--config"])
            .arg(&cfg)
            .env("SLATH__STORAGE__SOCKET_GROUP", &gid)
            .output()
            .unwrap();
        let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
        let socket = v["checks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["name"] == "socket")
            .unwrap()
            .clone();
        (
            socket["ok"].as_bool().unwrap(),
            socket["detail"].to_string(),
        )
    };
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o640)).unwrap();
    let (ok, detail) = check();
    assert!(ok, "a closed directory protects the key: {detail}");
    // Opening the directory to the group exposes the key (and the data
    // directory); whether it's
    // reachable also depends on the temp directory's ancestors.
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o750)).unwrap();
    let ancestors_open = dir.path().ancestors().skip(1).all(|d| {
        std::fs::metadata(d).is_ok_and(|m| {
            let bits = if m.gid().to_string() == gid {
                m.mode() >> 3
            } else {
                m.mode()
            };
            bits & 0o1 != 0
        })
    });
    let (ok, detail) = check();
    assert_eq!(ok, !ancestors_open, "{detail}");
    if ancestors_open {
        assert!(detail.contains("master.key"), "{detail}");
    }
    // Closing it again (the data directory inside is 0755) fixes both.
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let (ok, detail) = check();
    assert!(ok, "{detail}");
}

#[test]
fn config_check_rejects_missing_public_url() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = dir.path().join("slath.toml");
    std::fs::write(
        &cfg,
        "[storage]\ndata_dir = \"/d\"\nrun_dir = \"/r\"\n[server]\n",
    )
    .unwrap();
    let out = slath(&["config", "check", "--config", cfg.to_str().unwrap()]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("public_url"));
}

#[test]
fn config_check_fails_on_unreachable_database() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = dir.path().join("slath.toml");
    std::fs::write(
        &cfg,
        format!(
            "[server]\npublic_url = \"http://localhost\"\n[storage]\ndata_dir = \"{d}/data\"\n\
             run_dir = \"{d}/run\"\n[database]\nurl = \"postgres://x:y@127.0.0.1:1/none\"\n",
            d = dir.path().display()
        ),
    )
    .unwrap();
    let out = slath(&["config", "check", "--config", cfg.to_str().unwrap()]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stdout).contains("FAIL database"));
}
