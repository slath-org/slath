// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Diff helpers against a real Git (`/usr/bin/git` or `$SLATH_TEST_GIT`):
//! ignoring whitespace, generated-file attributes, range-diff, and the
//! stack reader for change identity.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;
use std::time::Duration;

use slath_git::browse::{self, DiffOptions};
use slath_git::{ObjectId, Runner, RunnerConfig};

struct Fixture {
    _dir: tempfile::TempDir,
    runner: Runner,
    repo: PathBuf,
}

async fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("git-home");
    std::fs::create_dir(&home).unwrap();
    let mut cfg = RunnerConfig::new(
        std::env::var_os("SLATH_TEST_GIT")
            .map_or_else(|| PathBuf::from("/usr/bin/git"), PathBuf::from),
        home,
    );
    cfg.read_timeout = Duration::from_secs(20);
    let runner = Runner::new(cfg).unwrap();
    let repo = dir.path().join("repo.git");
    runner
        .repo(&repo)
        .args(["init", "--bare", "--quiet", "--initial-branch=main"])
        .run()
        .await
        .unwrap();
    Fixture {
        _dir: dir,
        runner,
        repo,
    }
}

async fn oid(f: &Fixture, args: &[&str], stdin: Vec<u8>) -> ObjectId {
    let out = f
        .runner
        .repo(&f.repo)
        .args(args)
        .stdin(stdin)
        .run()
        .await
        .unwrap();
    ObjectId::parse(out.stdout.trim_ascii_end()).unwrap()
}

/// A commit with exactly these files on top of `parent`.
async fn commit(
    f: &Fixture,
    files: &[(&str, &str)],
    msg: &str,
    parent: Option<&ObjectId>,
) -> ObjectId {
    let mut tree = String::new();
    let mut sorted = files.to_vec();
    sorted.sort_unstable();
    for (name, text) in sorted {
        let blob = oid(
            f,
            &["hash-object", "-w", "--stdin"],
            text.as_bytes().to_vec(),
        )
        .await;
        tree.push_str(&format!("100644 blob {}\t{name}\n", blob.as_str()));
    }
    let tree = oid(f, &["mktree"], tree.into_bytes()).await;
    let mut args = vec![
        "-c",
        "user.name=T",
        "-c",
        "user.email=t@example.org",
        "commit-tree",
        tree.as_str(),
        "-m",
        msg,
    ];
    if let Some(p) = parent {
        args.extend(["-p", p.as_str()]);
    }
    oid(f, &args, Vec::new()).await
}

#[tokio::test]
async fn ignoring_whitespace() {
    let f = fixture().await;
    let a = commit(&f, &[("x", "a\nb\n")], "a", None).await;
    let b = commit(&f, &[("x", "  a\n  b\n")], "b", Some(&a)).await;
    let (plain, _) = browse::diff(
        &f.runner,
        &f.repo,
        Some(&a),
        &b,
        DiffOptions::default(),
        1 << 20,
    )
    .await
    .unwrap();
    assert!(String::from_utf8_lossy(&plain).contains("+  a"));
    let (ws, _) = browse::diff(
        &f.runner,
        &f.repo,
        Some(&a),
        &b,
        DiffOptions {
            ignore_whitespace: true,
        },
        1 << 20,
    )
    .await
    .unwrap();
    assert!(!String::from_utf8_lossy(&ws).contains("+  a"));
}

#[tokio::test]
async fn generated_files_from_the_commits_attributes() {
    let f = fixture().await;
    let c = commit(
        &f,
        &[
            (".gitattributes", "gen/* linguist-generated\nlock.json slath-generated\nkeep.txt -linguist-generated\n"),
            ("lock.json", "{}"),
            ("keep.txt", "k"),
            ("src.rs", "s"),
        ],
        "c",
        None,
    )
    .await;
    let paths: Vec<Vec<u8>> = ["gen/out.js", "lock.json", "keep.txt", "src.rs"]
        .iter()
        .map(|p| p.as_bytes().to_vec())
        .collect();
    let mut got: Vec<String> = browse::generated_paths(&f.runner, &f.repo, &c, &paths)
        .await
        .unwrap()
        .into_iter()
        .map(|p| String::from_utf8(p).unwrap())
        .collect();
    got.sort();
    assert_eq!(got, vec!["gen/out.js", "lock.json"]);
}

#[tokio::test]
async fn range_diff_pairs_rewritten_commits() {
    let f = fixture().await;
    let base = commit(&f, &[("x", "base\n")], "base", None).await;
    let lines = |edit: bool| -> String {
        (0..10)
            .map(|i| {
                if edit && i == 4 {
                    "one, fixed\n".to_owned()
                } else {
                    format!("line {i}\n")
                }
            })
            .collect()
    };
    let (y1, y2) = (lines(false), lines(true));
    let one = commit(&f, &[("x", "base\n"), ("y", &y1)], "Add y", Some(&base)).await;
    let rewritten = commit(&f, &[("x", "base\n"), ("y", &y2)], "Add y", Some(&base)).await;
    let (out, cut) = browse::range_diff(
        &f.runner,
        &f.repo,
        (&base, &one),
        (&base, &rewritten),
        60,
        1 << 20,
    )
    .await
    .unwrap();
    let text = String::from_utf8_lossy(&out);
    assert!(!cut);
    assert!(text.contains("1:") && text.contains(" ! 1:"), "{text}");
    assert!(text.contains("one, fixed"), "{text}");
}

#[tokio::test]
async fn stack_reads_ids_patches_and_conflicts() {
    let f = fixture().await;
    let base = commit(&f, &[("x", "base\n")], "base", None).await;
    let c = commit(
        &f,
        &[("x", "base\n"), ("y", "y\n")],
        "Add y\n\nChange-Id: I0123456789abcdef0123456789abcdef01234567",
        Some(&base),
    )
    .await;
    let conflict = commit(
        &f,
        &[
            ("x", "base\n"),
            (".jjconflict-base-0", "b"),
            (".jjconflict-side-0", "s"),
        ],
        "conflicted",
        Some(&c),
    )
    .await;
    let s = slath_git::changes::stack(&f.runner, &f.repo, &[c.clone(), conflict.clone()], 1 << 20)
        .await
        .unwrap();
    assert_eq!(
        s[0].trailer.as_deref(),
        Some("I0123456789abcdef0123456789abcdef01234567")
    );
    assert_eq!(s[0].parent.as_ref(), Some(&base));
    assert!(s[0].patch_id.as_deref().is_some_and(|p| p.len() == 40));
    assert!(String::from_utf8_lossy(&s[0].patch).contains("+y"));
    assert!(!s[0].conflicted);
    assert!(s[1].conflicted);
    // Without patches: IDs only.
    let s = slath_git::changes::stack(&f.runner, &f.repo, &[c], 0)
        .await
        .unwrap();
    assert!(s[0].patch.is_empty() && s[0].patch_id.is_none());
}
