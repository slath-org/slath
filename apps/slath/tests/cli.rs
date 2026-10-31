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
