// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Running the deterministic fixture generators in `tests/fixtures/`.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Directory holding the `make_*.sh` generators.
#[must_use]
pub fn dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures")
}

/// Names of all generators, e.g. `basic` for `make_basic.sh`.
#[must_use]
pub fn names() -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir())
        .unwrap()
        .filter_map(|e| {
            let name = e.unwrap().file_name().into_string().ok()?;
            Some(name.strip_prefix("make_")?.strip_suffix(".sh")?.to_owned())
        })
        .collect();
    names.sort();
    names
}

/// Generates fixture `name` as a work repository at `dest` (which must not exist).
/// `args` are extra generator arguments (e.g. the commit count for `deep_history`).
pub fn generate(name: &str, dest: &Path, args: &[&str]) {
    let script = dir().join(format!("make_{name}.sh"));
    let out = Command::new(&script).arg(dest).args(args).output().unwrap();
    assert!(
        out.status.success(),
        "{} failed:\n{}",
        script.display(),
        String::from_utf8_lossy(&out.stderr)
    );
}

/// All refs of a repository as `<oid> <refname>` lines (raw bytes, sorted by Git).
#[must_use]
pub fn refs(repo: &Path) -> Vec<u8> {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["for-each-ref", "--format=%(objectname) %(refname)"])
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap();
    assert!(out.status.success());
    out.stdout
}
