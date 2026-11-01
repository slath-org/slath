// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! The code browser: trees, files, raw files, history, commits, and refs,
//! including hostile names and paths.

use std::path::Path;

use slath_tests::fixtures;
use slath_tests::http::Client;
use slath_tests::server::{DEMO, GitClient, PASSWORD, TestServer};

fn push_fixture(s: &TestServer, name: &str) -> tempfile::TempDir {
    let d = tempfile::tempdir().unwrap();
    let src = d.path().join("src");
    fixtures::generate(name, &src, &[]);
    GitClient::new().ok(
        &src,
        &[
            "push",
            "-q",
            &s.url(&DEMO),
            "refs/heads/*:refs/heads/*",
            "refs/tags/*:refs/tags/*",
        ],
    );
    d
}

fn alice(s: &TestServer) -> Client {
    s.login("alice", PASSWORD)
}

/// A work tree with hostile content committed on `main` and pushed.
fn push_hostile(s: &TestServer) -> tempfile::TempDir {
    let git = GitClient::new();
    let d = tempfile::tempdir().unwrap();
    let w = d.path().join("w");
    std::fs::create_dir_all(w.join("dir")).unwrap();
    git.ok(&w, &["init", "-q", "-b", "main"]);
    let files: &[(&str, &[u8])] = &[
        ("README.md", b"# Hello\n\n<script>alert(1)</script>\n\n<div hx-get=\"/-/logout\" hx-trigger=\"load\">x</div>\n\n[x](javascript:alert(1))\n"),
        ("page.html", b"<!doctype html><script>alert(1)</script>"),
        ("image.svg", b"<svg xmlns=\"http://www.w3.org/2000/svg\" onload=\"alert(1)\"/>"),
        ("data.xml", b"<?xml version=\"1.0\"?><x/>"),
        ("note.txt", b"plain text\n"),
        ("pic.png", b"\x89PNG\r\n\x1a\n\x00\x00"),
        ("evil.png", b"<html><script>alert(1)</script></html>"),
        ("<img src=x onerror=alert(1)>.md", b"name\n"),
        ("dir/code.rs", b"fn main() { let s = \"\xe2\x80\xae\"; }\n"),
    ];
    for (name, data) in files {
        std::fs::write(w.join(name), data).unwrap();
    }
    std::os::unix::fs::symlink("/etc/passwd", w.join("passwd-link")).unwrap();
    git.ok(&w, &["add", "-A"]);
    git.ok(
        &w,
        &["commit", "-q", "-m", "<b>bold</b> \u{202e}gnp.exe subject"],
    );
    git.ok(&w, &["branch", "feature/deep/name"]);
    git.ok(&w, &["branch", "release"]);
    git.ok(&w, &["tag", "v1"]);
    git.ok(&w, &["tag", "release/1.0"]);
    git.ok(&w, &["push", "-q", &s.url(&DEMO), "--all"]);
    git.ok(&w, &["push", "-q", &s.url(&DEMO), "--tags"]);
    d
}

#[test]
fn conf_git_04_unicode_paths_and_names_render() {
    let s = TestServer::start();
    let _d = push_fixture(&s, "unicode");
    let mut a = alice(&s);
    let home = a.get("/alice/demo");
    assert_eq!(home.status, 200);
    assert!(
        home.body.contains("naïve café.txt") || home.body.contains("ünïcödé"),
        "{}",
        home.body
    );
    for path in [
        "/alice/demo/tree/main/%C3%BCn%C3%AFc%C3%B6d%C3%A9",
        "/alice/demo/blob/main/%C3%BCn%C3%AFc%C3%B6d%C3%A9/na%C3%AFve%20caf%C3%A9.txt",
        "/alice/demo/blob/main/latin1-%E9.txt",
        "/alice/demo/raw/main/latin1-%E9.txt",
        "/alice/demo/blob/main/nfd/cafe%CC%81.txt",
        "/alice/demo/blob/main/emoji/%F0%9F%A6%80.rs",
        "/alice/demo/blob/main/-leading-dash.txt",
        "/alice/demo/tree/caf%C3%A9",
        "/alice/demo/tree/latin1-%E9",
        "/alice/demo/commits",
        "/alice/demo/branches",
    ] {
        let r = a.get(path);
        assert_eq!(
            r.status,
            200,
            "{path}: {}",
            &r.body[..r.body.len().min(300)]
        );
    }
    assert_eq!(
        a.get("/alice/demo/raw/main/latin1-%E9.txt").body,
        "non-UTF-8 file name\n"
    );
}

#[test]
fn conf_git_04_branches_with_slashes_and_longest_match() {
    let s = TestServer::start();
    let _d = push_hostile(&s);
    let mut a = alice(&s);
    // The whole "feature/deep/name" is the branch; "dir" is the path.
    let r = a.get("/alice/demo/tree/feature/deep/name/dir");
    assert_eq!(r.status, 200);
    assert!(
        r.body.contains("Branch <strong>feature/deep/name</strong>"),
        "{}",
        r.body
    );
    assert_eq!(
        a.get("/alice/demo/tree/feature/dir").status,
        404,
        "no branch is called feature"
    );
    // Branches before tags: "release/1.0" is branch "release" with path "1.0".
    assert_eq!(a.get("/alice/demo/tree/release/1.0").status, 404);
    assert!(
        a.get("/alice/demo/tree/release/dir")
            .body
            .contains("Branch <strong>release</strong>")
    );
    assert_eq!(a.get("/alice/demo/tree/v1").status, 200, "tags resolve too");
    let oid =
        s.db.query_one("SELECT oid FROM git_refs WHERE name = 'refs/heads/main'::bytea");
    assert_eq!(
        a.get(&format!("/alice/demo/tree/{oid}/dir")).status,
        200,
        "full commit IDs resolve"
    );
    let commit = a.get(&format!("/alice/demo/commit/{oid}"));
    assert_eq!(commit.status, 200);
    assert!(
        commit.body.contains("&#60;b&#62;bold&#60;/b&#62;"),
        "subjects are escaped"
    );
    assert!(
        commit.body.contains("[U+202E]"),
        "bidi controls are visible"
    );
    assert!(!commit.body.contains('\u{202e}'));
}

#[test]
fn conf_sec_04_raw_files_are_isolated() {
    let s = TestServer::start();
    let _d = push_hostile(&s);
    let mut a = alice(&s);
    for (path, ct, attachment) in [
        ("page.html", "application/octet-stream", true),
        ("image.svg", "application/octet-stream", true),
        ("data.xml", "application/octet-stream", true),
        ("evil.png", "application/octet-stream", true),
        ("note.txt", "text/plain; charset=utf-8", false),
        ("pic.png", "image/png", false),
        ("passwd-link", "text/plain; charset=utf-8", false),
    ] {
        let r = a.get(&format!(
            "/alice/demo/raw/main/{path}?content_type=text/html"
        ));
        assert_eq!(r.status, 200, "{path}");
        assert_eq!(r.header("content-type"), Some(ct), "{path}");
        assert_eq!(
            r.header("content-disposition").is_some(),
            attachment,
            "{path}"
        );
        assert_eq!(
            r.header("x-content-type-options"),
            Some("nosniff"),
            "{path}"
        );
        assert!(
            r.header("content-security-policy")
                .unwrap()
                .starts_with("sandbox; default-src 'none'"),
            "{path}"
        );
        assert_eq!(
            r.header("cache-control"),
            Some("private, no-store"),
            "{path}: private repository"
        );
    }
    // A symlink's raw content is its target text, never the target.
    assert_eq!(
        a.get("/alice/demo/raw/main/passwd-link").body,
        "/etc/passwd"
    );
    assert_eq!(a.get("/alice/demo/raw/main/passwd-link/x").status, 404);
    let blob = a.get("/alice/demo/blob/main/passwd-link");
    assert!(blob.body.contains("Symbolic link to /etc/passwd"));
    assert!(!blob.body.contains("root:"));
}

#[test]
fn conf_sec_04_user_content_is_escaped_or_sanitized() {
    let s = TestServer::start();
    let _d = push_hostile(&s);
    let mut a = alice(&s);
    let home = a.get("/alice/demo");
    assert!(!home.body.contains("<script>alert"), "README is sanitized");
    assert!(!home.body.contains("hx-get=\"/-/logout\""));
    assert!(!home.body.contains("javascript:"));
    assert!(
        home.body.contains("class=\"markdown\" hx-ignore"),
        "user HTML sits in hx-ignore"
    );
    assert!(
        home.body
            .contains("&#60;img src=x onerror=alert(1)&#62;.md"),
        "file names are escaped"
    );
    let code = a.get("/alice/demo/blob/main/dir/code.rs");
    assert!(code.body.contains("[U+202E]"));
    assert!(code.body.contains("class=\"syn-"));
}

#[test]
fn conf_sec_01_revision_tricks_and_traversal_find_nothing() {
    let s = TestServer::start();
    let _d = push_hostile(&s);
    let mut a = alice(&s);
    for path in [
        "/alice/demo/tree/main%5E%7Btree%7D",
        "/alice/demo/tree/main@%7B1%7D",
        "/alice/demo/tree/HEAD",
        "/alice/demo/tree/main~1",
        "/alice/demo/tree/:/secret",
        "/alice/demo/tree/FETCH_HEAD",
        "/alice/demo/tree/--output=%2Ftmp%2Fslath-pwned",
        "/alice/demo/blob/--upload-pack=touch%20x/README.md",
        "/alice/demo/commits/--all",
        "/alice/demo/commit/main",
        "/alice/demo/commit/--output=x",
        "/alice/demo/commit/abc1234",
        "/alice/demo/raw/main/../../../../etc/passwd",
        "/alice/demo/raw/main/..%2f..%2fetc%2fpasswd",
        "/alice/demo/raw/main/%2e%2e/%2e%2e/etc/passwd",
        "/alice/demo/raw/main//etc/passwd",
        "/alice/demo/raw/main/dir/..%5c..%5cetc",
        "/alice/demo/raw/main/README.md%00",
        "/alice/demo/raw/main/README.md/..",
    ] {
        let r = a.get(path);
        assert_eq!(r.status, 404, "{path}");
        assert!(!r.body.contains("root:x:0"), "{path}");
    }
    assert!(!Path::new("/tmp/slath-pwned").exists());
}

#[test]
fn diff_truncation_is_announced() {
    let s = TestServer::start_full(&[DEMO], "[limits]\ndiff_max_files = 2\n");
    let _d = push_hostile(&s);
    let mut a = alice(&s);
    let oid =
        s.db.query_one("SELECT oid FROM git_refs WHERE name = 'refs/heads/main'::bytea");
    let r = a.get(&format!("/alice/demo/commit/{oid}"));
    assert!(
        r.body.contains("too large to show completely"),
        "{}",
        r.body
    );
    assert!(r.body.contains("2 changed files"));
}

#[test]
fn conf_git_03_browser_routes_hide_private_repositories() {
    let s = TestServer::start();
    let _d = push_hostile(&s);
    s.create_user("mallory", PASSWORD, false);
    let mut m = s.login("mallory", PASSWORD);
    let mut anon = s.client();
    let oid =
        s.db.query_one("SELECT oid FROM git_refs WHERE name = 'refs/heads/main'::bytea");
    for c in [&mut m, &mut anon] {
        for path in [
            "/alice/demo/tree/main".to_owned(),
            "/alice/demo/blob/main/note.txt".to_owned(),
            "/alice/demo/raw/main/note.txt".to_owned(),
            "/alice/demo/commits".to_owned(),
            format!("/alice/demo/commit/{oid}"),
            "/alice/demo/branches".to_owned(),
            "/alice/demo/tags".to_owned(),
        ] {
            let r = c.get(&path);
            assert_eq!(r.status, 404, "{path}");
            assert!(!r.body.contains("plain text"), "{path}");
        }
    }
}

#[test]
fn conf_git_03_archives_match_git_archive_and_respect_access() {
    let s = TestServer::start();
    let _d = push_fixture(&s, "binary");
    let mut a = alice(&s);
    let oid =
        s.db.query_one("SELECT oid FROM git_refs WHERE name = 'refs/heads/main'::bytea");
    let short = &oid[..12];
    let zip = a.get("/alice/demo/archive/main.zip");
    assert_eq!(zip.status, 200);
    assert_eq!(zip.header("content-type"), Some("application/zip"));
    assert_eq!(
        zip.header("content-disposition"),
        Some(format!("attachment; filename=\"demo-{short}.zip\"").as_str())
    );
    assert_eq!(zip.header("cache-control"), Some("private, no-store"));
    let git = GitClient::new();
    let want = git
        .cmd(&s.repo_path(&DEMO))
        .args([
            "archive",
            "--format=zip",
            &format!("--prefix=demo-{short}/"),
            &oid,
        ])
        .output()
        .unwrap();
    assert!(want.status.success());
    assert_eq!(zip.bytes, want.stdout, "identical to git archive");
    let tgz = a.get(&format!("/alice/demo/archive/{oid}.tar.gz"));
    assert_eq!(tgz.status, 200);
    let mut tar = Vec::new();
    std::io::Read::read_to_end(&mut flate2::read::GzDecoder::new(&tgz.bytes[..]), &mut tar)
        .unwrap();
    let want = git
        .cmd(&s.repo_path(&DEMO))
        .args([
            "archive",
            "--format=tar",
            &format!("--prefix=demo-{short}/"),
            &oid,
        ])
        .output()
        .unwrap();
    assert_eq!(tar, want.stdout);
    // Unknown refs, paths, formats, and option-like names find nothing.
    for p in [
        "/alice/demo/archive/nope.zip",
        "/alice/demo/archive/main/bin.zip",
        "/alice/demo/archive/main.tar",
        "/alice/demo/archive/--output=x.zip",
        "/alice/demo/archive/--exec=id.tar.gz",
    ] {
        assert_eq!(a.get(p).status, 404, "{p}");
    }
    // Private repositories: nothing for others.
    s.create_user("mallory", PASSWORD, false);
    let mut m = s.login("mallory", PASSWORD);
    assert_eq!(m.get("/alice/demo/archive/main.zip").status, 404);
    assert_eq!(
        s.client().get("/alice/demo/archive/main.tar.gz").status,
        404
    );
}
