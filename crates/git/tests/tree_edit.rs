// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Replacing one file in a commit's tree without a worktree (applying
//! review suggestions). Needs a system Git at `/usr/bin/git` (or
//! `$SLATH_TEST_GIT`).
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use slath_git::merge::replace_file;
use slath_git::{ObjectId, Runner, RunnerConfig};

fn git(dir: &Path, args: &[&str]) -> String {
    let git = std::env::var_os("SLATH_TEST_GIT").unwrap_or_else(|| "/usr/bin/git".into());
    let out = Command::new(git)
        .current_dir(dir)
        .args(["-c", "user.name=T", "-c", "user.email=t@example.org"])
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap().trim().to_owned()
}

#[tokio::test]
async fn replace_file_keeps_everything_else() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    std::fs::create_dir(&home).unwrap();
    let mut cfg = RunnerConfig::new(
        std::env::var_os("SLATH_TEST_GIT")
            .map_or_else(|| PathBuf::from("/usr/bin/git"), PathBuf::from),
        home,
    );
    cfg.read_timeout = Duration::from_secs(20);
    let runner = Runner::new(cfg).unwrap();
    let w = dir.path().join("w");
    std::fs::create_dir_all(w.join("src/deep")).unwrap();
    std::fs::write(w.join("README"), "top\n").unwrap();
    std::fs::write(w.join("src/deep/a.txt"), "one\ntwo\n").unwrap();
    std::fs::write(w.join("src/run.sh"), "#!/bin/sh\n").unwrap();
    git(&w, &["init", "-q", "-b", "main"]);
    git(&w, &["add", "."]);
    git(&w, &["update-index", "--chmod=+x", "src/run.sh"]);
    git(&w, &["commit", "-q", "-m", "init"]);
    let head = ObjectId::parse(git(&w, &["rev-parse", "HEAD"]).as_bytes()).unwrap();
    let gitdir = w.join(".git");

    let tree = replace_file(
        &runner,
        &gitdir,
        &head,
        b"src/deep/a.txt",
        b"one\nTWO\n".to_vec(),
    )
    .await
    .unwrap()
    .unwrap();
    let t = tree.as_str();
    assert_eq!(
        git(&w, &["cat-file", "blob", &format!("{t}:src/deep/a.txt")]),
        "one\nTWO"
    );
    assert_eq!(
        git(&w, &["cat-file", "blob", &format!("{t}:README")]),
        "top"
    );
    // Untouched entries keep their objects and modes.
    assert_eq!(
        git(&w, &["ls-tree", t, "README"]),
        git(&w, &["ls-tree", "HEAD", "README"])
    );
    let exe = replace_file(
        &runner,
        &gitdir,
        &head,
        b"src/run.sh",
        b"#!/bin/sh\nexit 0\n".to_vec(),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(git(&w, &["ls-tree", exe.as_str(), "src/run.sh"]).starts_with("100755 "));
    // Missing files, directories, and odd paths aren't replaced.
    for p in [&b"nope"[..], b"src", b"src/../README", b"src//run.sh"] {
        assert!(
            replace_file(&runner, &gitdir, &head, p, Vec::new())
                .await
                .unwrap()
                .is_none(),
            "{p:?}"
        );
    }
}
