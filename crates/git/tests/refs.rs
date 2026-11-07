// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Server-side ref writes: compare-and-swap updates and merge request pins
//! (GIT-09). They need a system Git at `/usr/bin/git` (or `$SLATH_TEST_GIT`).
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;
use std::time::Duration;

use slath_core::ids::MergeRequestId;
use slath_git::args::RefName;
use slath_git::refs::{self, RefUpdateError};
use slath_git::{ObjectId, ObjectReader, Runner, RunnerConfig};

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

/// A commit with one file whose content is `text`, on top of `parent`.
async fn commit(f: &Fixture, text: &str, parent: Option<&ObjectId>) -> ObjectId {
    let run = |args: Vec<String>, stdin: Vec<u8>| {
        let inv = f.runner.repo(&f.repo).args(args).stdin(stdin);
        async move {
            let out = inv.run().await.unwrap();
            ObjectId::parse(out.stdout.trim_ascii_end()).unwrap()
        }
    };
    let blob = run(
        vec!["hash-object".into(), "-w".into(), "--stdin".into()],
        text.as_bytes().to_vec(),
    )
    .await;
    let tree = run(
        vec!["mktree".into()],
        format!("100644 blob {}\tfile.txt\n", blob.as_str()).into_bytes(),
    )
    .await;
    let mut args: Vec<String> = [
        "-c",
        "user.name=T",
        "-c",
        "user.email=t@example.org",
        "commit-tree",
    ]
    .map(str::to_owned)
    .to_vec();
    args.push(tree.as_str().to_owned());
    if let Some(p) = parent {
        args.extend(["-p".to_owned(), p.as_str().to_owned()]);
    }
    args.extend(["-m".to_owned(), text.to_owned()]);
    let inv = f.runner.repo(&f.repo).args(args);
    ObjectId::parse(inv.run().await.unwrap().stdout.trim_ascii_end()).unwrap()
}

#[tokio::test]
async fn update_cas_creates_moves_and_deletes_only_from_the_expected_value() {
    let f = fixture().await;
    let a = commit(&f, "a", None).await;
    let b = commit(&f, "b", Some(&a)).await;
    let main = RefName::branch(b"main").unwrap();

    // Create: must not exist yet.
    refs::update_cas(&f.runner, &f.repo, &main, Some(&a), None)
        .await
        .unwrap();
    assert_eq!(
        refs::read(&f.runner, &f.repo, &main).await.unwrap(),
        Some(a.clone())
    );
    match refs::update_cas(&f.runner, &f.repo, &main, Some(&b), None).await {
        Err(RefUpdateError::Conflict { current }) => assert_eq!(current, Some(a.clone())),
        other => panic!("{other:?}"),
    }
    // Move: from the expected value only.
    match refs::update_cas(&f.runner, &f.repo, &main, Some(&b), Some(&b)).await {
        Err(RefUpdateError::Conflict { current }) => assert_eq!(current, Some(a.clone())),
        other => panic!("{other:?}"),
    }
    refs::update_cas(&f.runner, &f.repo, &main, Some(&b), Some(&a))
        .await
        .unwrap();
    // Delete.
    assert!(matches!(
        refs::update_cas(&f.runner, &f.repo, &main, None, Some(&a)).await,
        Err(RefUpdateError::Conflict { .. })
    ));
    refs::update_cas(&f.runner, &f.repo, &main, None, Some(&b))
        .await
        .unwrap();
    assert_eq!(refs::read(&f.runner, &f.repo, &main).await.unwrap(), None);

    // `read` is exact: a longer name with the same prefix isn't the ref.
    let nested = RefName::parse(&b"refs/heads/main2"[..]).unwrap();
    refs::set(&f.runner, &f.repo, &nested, &a).await.unwrap();
    assert_eq!(refs::read(&f.runner, &f.repo, &main).await.unwrap(), None);
    refs::set(&f.runner, &f.repo, &nested, &b).await.unwrap();
    assert_eq!(
        refs::read(&f.runner, &f.repo, &nested).await.unwrap(),
        Some(b)
    );
}

#[tokio::test]
async fn conf_git_09_pinned_revisions_survive_force_push_and_gc() {
    let f = fixture().await;
    let mr = MergeRequestId::new();
    let branch = RefName::branch(b"topic").unwrap();
    let base = commit(&f, "base", None).await;
    let mut heads = Vec::new();
    let mut expected = None;
    // Three revisions, each a force push that drops the previous head.
    for (seq, text) in [(1, "one"), (2, "two"), (3, "three")] {
        let head = commit(&f, text, Some(&base)).await;
        refs::update_cas(&f.runner, &f.repo, &branch, Some(&head), expected.as_ref())
            .await
            .unwrap();
        refs::pin_revision(&f.runner, &f.repo, mr, seq, &head)
            .await
            .unwrap();
        // Pinning again is a no-op; pinning elsewhere is refused.
        refs::pin_revision(&f.runner, &f.repo, mr, seq, &head)
            .await
            .unwrap();
        assert!(
            refs::pin_revision(&f.runner, &f.repo, mr, seq, &base)
                .await
                .is_err()
        );
        refs::set(&f.runner, &f.repo, &refs::mr_head(7), &head)
            .await
            .unwrap();
        heads.push(head.clone());
        expected = Some(head);
    }
    f.runner
        .repo(&f.repo)
        .args(["reflog", "expire", "--expire=now", "--all"])
        .run()
        .await
        .unwrap();
    f.runner
        .repo(&f.repo)
        .args(["gc", "--prune=now", "--quiet"])
        .run()
        .await
        .unwrap();
    let mut reader = ObjectReader::open(&f.runner, &f.repo).await.unwrap();
    for head in &heads {
        reader.read_commit(head).await.unwrap();
    }
    assert_eq!(
        refs::read(&f.runner, &f.repo, &refs::mr_head(7))
            .await
            .unwrap(),
        heads.last().cloned()
    );

    // Removing the merge request's refs lets GC collect the old revisions.
    refs::remove_mr_refs(&f.runner, &f.repo, mr, 7)
        .await
        .unwrap();
    assert_eq!(
        refs::read(&f.runner, &f.repo, &refs::mr_head(7))
            .await
            .unwrap(),
        None
    );
    assert_eq!(
        refs::read(&f.runner, &f.repo, &refs::mr_revision_pin(mr, 1))
            .await
            .unwrap(),
        None
    );
    f.runner
        .repo(&f.repo)
        .args(["gc", "--prune=now", "--quiet"])
        .run()
        .await
        .unwrap();
    assert!(
        !refs::commit_exists(&f.runner, &f.repo, &heads[0])
            .await
            .unwrap()
    );
    assert!(
        refs::commit_exists(&f.runner, &f.repo, &heads[2])
            .await
            .unwrap()
    );
}
