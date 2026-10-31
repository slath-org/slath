// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Integration tests for the hardened runner and the object reader.
//! They need a system Git at `/usr/bin/git` (or `$SLATH_TEST_GIT`).
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use slath_git::commit::RawCommit;
use slath_git::object::{Blob, EntryKind, ObjectType};
use slath_git::{GitError, LimitKind, ObjectId, ObjectReader, Runner, RunnerConfig};

fn git_bin() -> PathBuf {
    std::env::var_os("SLATH_TEST_GIT").map_or_else(|| PathBuf::from("/usr/bin/git"), PathBuf::from)
}

struct Fixture {
    _dir: tempfile::TempDir,
    runner: Runner,
    repo: PathBuf,
    home: PathBuf,
}

async fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("git-home");
    std::fs::create_dir(&home).unwrap();
    let mut cfg = RunnerConfig::new(git_bin(), home.clone());
    cfg.read_timeout = Duration::from_secs(10);
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
        home,
    }
}

async fn hash_object(f: &Fixture, kind: &str, data: &[u8]) -> ObjectId {
    let out = f
        .runner
        .repo(&f.repo)
        .args(["hash-object", "-t", kind, "-w", "--literally", "--stdin"])
        .stdin(data.to_vec())
        .run()
        .await
        .unwrap();
    ObjectId::parse(out.stdout.trim_ascii_end()).unwrap()
}

#[tokio::test]
async fn version_check_passes_on_supported_git() {
    let f = fixture().await;
    let v = f.runner.check_version().await.unwrap();
    assert!(v.major >= 2);
}

/// Re-runs this test binary with a hostile environment; the child half then
/// checks that none of it reaches Git.
#[tokio::test]
async fn inherited_environment_does_not_leak() {
    if std::env::var_os("SLATH_TEST_ENV_CHILD").is_some() {
        let f = fixture().await;
        let out = f
            .runner
            .repo(&f.repo)
            .args(["-c", "alias.showenv=!env", "showenv"])
            .run()
            .await
            .unwrap();
        let env = String::from_utf8(out.stdout).unwrap();
        for line in env.lines() {
            assert!(!line.contains("/evil"), "leaked: {line}");
            assert!(!line.starts_with("GIT_CONFIG_COUNT"), "leaked: {line}");
            assert!(!line.starts_with("SLATH_TEST_SECRET"), "leaked: {line}");
        }
        assert!(
            env.lines()
                .any(|l| l == format!("HOME={}", f.home.display()))
        );
        assert!(env.lines().any(|l| l == "GIT_CONFIG_GLOBAL=/dev/null"));
        assert!(env.lines().any(|l| l == "GIT_CONFIG_NOSYSTEM=1"));
        assert!(env.lines().any(|l| l == "LC_ALL=C"));
        return;
    }
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "inherited_environment_does_not_leak",
            "--nocapture",
        ])
        .env("SLATH_TEST_ENV_CHILD", "1")
        .env("GIT_DIR", "/evil/repo")
        .env("GIT_WORK_TREE", "/evil/tree")
        .env("GIT_CONFIG_COUNT", "1")
        .env("GIT_CONFIG_KEY_0", "core.sshCommand")
        .env("GIT_CONFIG_VALUE_0", "/evil/ssh")
        .env("GIT_CONFIG_GLOBAL", "/evil/gitconfig")
        .env("HOME", "/evil/home")
        .env("SLATH_TEST_SECRET", "x")
        .status()
        .unwrap();
    assert!(status.success());
}

#[tokio::test]
async fn hanging_process_is_killed_at_timeout_without_orphans() {
    let f = fixture().await;
    let marker = format!("{}", 100_000 + std::process::id());
    let started = Instant::now();
    let err = f
        .runner
        .repo(&f.repo)
        // A shell alias whose grandchild would outlive git if only the direct
        // child were killed.
        .args([
            "-c",
            &format!("alias.hang=!sleep {marker} & sleep {marker}; :"),
            "hang",
        ])
        .timeout(Duration::from_millis(500))
        .output()
        .await
        .unwrap_err();
    assert!(
        matches!(err, GitError::Limit(LimitKind::Timeout)),
        "{err:?}"
    );
    assert!(started.elapsed() < Duration::from_secs(5));
    // Give the kernel a moment to deliver SIGKILL.
    for _ in 0..50 {
        if !process_with_arg_exists(&marker) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("orphaned `sleep {marker}` survived the timeout");
}

fn process_with_arg_exists(arg: &str) -> bool {
    let Ok(dir) = std::fs::read_dir("/proc") else {
        return false;
    };
    dir.flatten().any(|e| {
        let cmdline = std::fs::read(e.path().join("cmdline")).unwrap_or_default();
        let stat = std::fs::read_to_string(e.path().join("stat")).unwrap_or_default();
        // Ignore zombies, which are dead but not yet reaped.
        let zombie = stat
            .rsplit(')')
            .next()
            .is_some_and(|s| s.trim_start().starts_with('Z'));
        !zombie && cmdline.split(|&b| b == 0).any(|a| a == arg.as_bytes())
    })
}

#[tokio::test]
async fn output_cap_triggers_limit_error() {
    let f = fixture().await;
    let oid = hash_object(&f, "blob", &vec![b'x'; 100_000]).await;
    let err = f
        .runner
        .repo(&f.repo)
        .args(["cat-file", "blob", "--end-of-options", oid.as_str()])
        .max_output(1000)
        .output()
        .await
        .unwrap_err();
    assert!(matches!(err, GitError::Limit(LimitKind::Output)), "{err:?}");
}

#[tokio::test]
async fn failed_command_reports_status_and_stderr() {
    let f = fixture().await;
    let err = f
        .runner
        .repo(&f.repo)
        .args([
            "rev-parse",
            "--verify",
            "--end-of-options",
            "refs/heads/nope^{commit}",
        ])
        .run()
        .await
        .unwrap_err();
    assert!(matches!(err, GitError::Failed { .. }), "{err:?}");
}

#[tokio::test]
async fn hooks_are_disabled_outside_receive_pack() {
    let f = fixture().await;
    let out = f
        .runner
        .repo(&f.repo)
        .args(["config", "--get", "core.hooksPath"])
        .run()
        .await
        .unwrap();
    assert_eq!(out.stdout.trim_ascii_end(), b"/dev/null");
}

#[tokio::test]
async fn object_reader_reads_blobs_trees_and_commits() {
    let f = fixture().await;
    let blob = hash_object(&f, "blob", b"hello\n").await;
    let mut tree_raw = b"100755 run.sh\0".to_vec();
    tree_raw.extend_from_slice(&hex_to_bytes(blob.as_str()));
    let tree = hash_object(&f, "tree", &tree_raw).await;
    let commit_raw = format!(
        "tree {tree}\nauthor A <a@example.org> 1700000000 +0000\n\
         committer A <a@example.org> 1700000000 +0000\nchange-id {}\n\nmsg\n",
        "z".repeat(32)
    );
    let commit = hash_object(&f, "commit", commit_raw.as_bytes()).await;

    let mut r = ObjectReader::open(&f.runner, &f.repo).await.unwrap();
    assert_eq!(r.info(&blob).await.unwrap(), Some((ObjectType::Blob, 6)));
    assert_eq!(
        r.read_blob(&blob, 100).await.unwrap(),
        Blob::Data(b"hello\n".to_vec())
    );
    assert_eq!(
        r.read_blob(&blob, 3).await.unwrap(),
        Blob::TooLarge { size: 6 }
    );
    let entries = r.read_tree(&tree).await.unwrap();
    assert_eq!(entries[0].name, b"run.sh");
    assert_eq!(entries[0].kind(), EntryKind::Executable);
    assert_eq!(entries[0].oid, blob);
    let c = r.read_raw_commit(&commit).await.unwrap();
    assert_eq!(c.to_bytes(), commit_raw.as_bytes());
    assert_eq!(c.change_id(), Some("z".repeat(32).as_str()));
    // Wrong type and missing objects are not found; the reader stays usable.
    assert!(matches!(r.read_tree(&blob).await, Err(GitError::NotFound)));
    let missing = ObjectId::parse("1".repeat(40)).unwrap();
    assert_eq!(r.info(&missing).await.unwrap(), None);
    assert!(matches!(
        r.read_blob(&missing, 10).await,
        Err(GitError::NotFound)
    ));
    assert!(r.info(&blob).await.unwrap().is_some());
}

/// Commits with signatures, mergetags, encodings, and change IDs must
/// survive parse → serialize byte for byte, as read back from Git.
#[tokio::test]
async fn commit_parser_round_trips_real_objects() {
    let f = fixture().await;
    let tree = hash_object(&f, "tree", b"").await;
    let id =
        "\nauthor A <a@example.org> 1700000000 +0100\ncommitter Ç <c@example.org> 1700000001 -0230";
    let fixtures = [
        format!("tree {tree}{id}\n\nplain\n"),
        format!(
            "tree {tree}{id}\ngpgsig -----BEGIN PGP SIGNATURE-----\n \n iQEz\n =abcd\n \
             -----END PGP SIGNATURE-----\n\nsigned\n"
        ),
        format!(
            "tree {tree}\nparent {tree}\nparent {tree}{id}\nmergetag object {tree}\n type commit\n \
             tag v1\n tagger T <t@x> 1 +0000\n \n tag message\n\nMerge tag 'v1'\n"
        ),
        format!("tree {tree}{id}\nencoding ISO-8859-1\n\nlatin1 \u{1}\n"),
        format!(
            "tree {tree}{id}\nchange-id {}\nx-custom a\n\nno trailing newline",
            "m".repeat(32)
        ),
        format!("tree {tree}{id}\njj:trees {tree} {tree}\n\n"),
    ];
    let mut r = ObjectReader::open(&f.runner, &f.repo).await.unwrap();
    let mut fixtures: Vec<Vec<u8>> = fixtures.into_iter().map(String::into_bytes).collect();
    // Non-UTF-8 message bytes (ISO-8859-1 'é').
    for raw in &mut fixtures {
        if let Some(pos) = raw.iter().position(|&b| b == 1) {
            raw[pos] = 0xe9;
        }
    }
    for raw in fixtures {
        let oid = hash_object(&f, "commit", &raw).await;
        let c = r.read_raw_commit(&oid).await.unwrap();
        assert_eq!(c.to_bytes(), raw);
        assert_eq!(RawCommit::parse(c.to_bytes()).unwrap(), c);
    }
}

#[tokio::test]
async fn concurrency_is_bounded_by_the_semaphore() {
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = RunnerConfig::new(git_bin(), dir.path().to_path_buf());
    cfg.max_concurrent = 1;
    let runner = Runner::new(cfg).unwrap();
    let _held = runner
        .global()
        .arg("cat-file")
        .arg("--batch")
        .spawn()
        .await
        .unwrap();
    let second = tokio::time::timeout(
        Duration::from_millis(200),
        runner.global().arg("--version").run(),
    )
    .await;
    assert!(second.is_err(), "second process must wait for a permit");
}

#[test]
fn relative_paths_are_rejected() {
    assert!(Runner::new(RunnerConfig::new("git".into(), "/tmp".into())).is_err());
    assert!(
        Runner::new(RunnerConfig::new(
            "/usr/bin/git".into(),
            Path::new("rel").into()
        ))
        .is_err()
    );
}

fn hex_to_bytes(hex: &str) -> Vec<u8> {
    (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
        .collect()
}
