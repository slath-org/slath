// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Clone, fetch, and push over smart HTTP with protocol v0 and v2.

use std::io::Write;
use std::path::Path;

use slath_tests::fixtures;
use slath_tests::server::{DEMO, GitClient, TestServer};

/// `<oid> <ref>` lines for branches and tags.
fn heads_and_tags(git: &GitClient, repo: &Path) -> String {
    git.ok(
        repo,
        &[
            "for-each-ref",
            "--format=%(objectname) %(refname)",
            "refs/heads",
            "refs/tags",
        ],
    )
}

fn fsck_strict(git: &GitClient, bare: &Path) {
    git.ok(bare, &["fsck", "--strict", "--no-dangling"]);
}

fn round_trip(protocol: &str) {
    let server = TestServer::start();
    let git = GitClient::new();
    let work = tempfile::tempdir().unwrap();
    let src = work.path().join("src");
    fixtures::generate("tags", &src, &[]);
    let proto = format!("protocol.version={protocol}");
    let url = server.url(&DEMO);

    // Push every branch and tag of the fixture.
    git.ok(
        &src,
        &[
            "-c",
            &proto,
            "push",
            &url,
            "refs/heads/*:refs/heads/*",
            "refs/tags/*:refs/tags/*",
        ],
    );
    let expected = heads_and_tags(&git, &src);
    assert_eq!(heads_and_tags(&git, &server.repo_path(&DEMO)), expected);

    // Clone: identical refs and objects.
    git.ok(
        work.path(),
        &[
            "-c",
            &proto,
            "clone",
            "--quiet",
            "--mirror",
            &url,
            "clone.git",
        ],
    );
    let clone = work.path().join("clone.git");
    assert_eq!(heads_and_tags(&git, &clone), expected);
    fsck_strict(&git, &clone);

    // Push a new commit from a second work tree, then fetch it into the first.
    git.ok(work.path(), &["-c", &proto, "clone", "--quiet", &url, "w2"]);
    let w2 = work.path().join("w2");
    std::fs::write(w2.join("new.txt"), "new\n").unwrap();
    git.ok(&w2, &["add", "new.txt"]);
    git.ok(&w2, &["commit", "-q", "-m", "New commit"]);
    git.ok(
        &w2,
        &[
            "-c",
            &proto,
            "push",
            "origin",
            "HEAD:refs/heads/main",
            "HEAD:refs/heads/new-branch",
        ],
    );
    let head = git.ok(&w2, &["rev-parse", "HEAD"]);

    git.ok(
        &src,
        &[
            "-c",
            &proto,
            "fetch",
            "--quiet",
            &url,
            "+refs/heads/*:refs/remotes/slath/*",
        ],
    );
    assert_eq!(
        git.ok(&src, &["rev-parse", "refs/remotes/slath/new-branch"]),
        head
    );

    // Deleting a branch works too.
    git.ok(
        &w2,
        &["-c", &proto, "push", "origin", ":refs/heads/new-branch"],
    );
    let refs = heads_and_tags(&git, &server.repo_path(&DEMO));
    assert!(!refs.contains("refs/heads/new-branch"));
    assert!(refs.contains(&format!("{} refs/heads/main", head.trim())));
    fsck_strict(&git, &server.repo_path(&DEMO));
}

#[test]
fn conf_git_01_clone_fetch_push_protocol_v0() {
    round_trip("0");
}

#[test]
fn conf_git_01_clone_fetch_push_protocol_v2() {
    round_trip("2");
}

#[test]
fn conf_git_01_v2_is_negotiated() {
    let server = TestServer::start();
    let git = GitClient::new();
    let work = tempfile::tempdir().unwrap();
    let out = git
        .cmd(work.path())
        .env("GIT_TRACE_PACKET", "1")
        .args(["-c", "protocol.version=2", "ls-remote", &server.url(&DEMO)])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let trace = String::from_utf8_lossy(&out.stderr);
    assert!(
        trace.contains("< version 2"),
        "server did not speak v2:\n{trace}"
    );
}

#[test]
fn conf_git_01_fixtures_round_trip_unchanged() {
    // Binary files, symlinks, exec bits, Unicode and non-UTF-8 names, merges,
    // signatures, and submodules survive the trip byte for byte.
    for name in [
        "basic",
        "binary",
        "unicode",
        "merges",
        "signed",
        "submodule",
    ] {
        let server = TestServer::start();
        let git = GitClient::new();
        let work = tempfile::tempdir().unwrap();
        let src = work.path().join("src");
        fixtures::generate(name, &src, &[]);
        let url = server.url(&DEMO);
        git.ok(
            &src,
            &[
                "push",
                "--quiet",
                &url,
                "refs/heads/*:refs/heads/*",
                "refs/tags/*:refs/tags/*",
            ],
        );
        git.ok(
            work.path(),
            &["clone", "--quiet", "--mirror", &url, "clone.git"],
        );
        let clone = work.path().join("clone.git");
        assert_eq!(
            heads_and_tags(&git, &clone),
            heads_and_tags(&git, &src),
            "{name}"
        );
        fsck_strict(&git, &server.repo_path(&DEMO));
    }
}

#[test]
fn conf_git_01_reserved_refs_are_denied() {
    let server = TestServer::start();
    let git = GitClient::new();
    let work = tempfile::tempdir().unwrap();
    let src = work.path().join("src");
    fixtures::generate("basic", &src, &[]);
    let url = server.url(&DEMO);
    for target in ["HEAD:refs/slath/x", "HEAD:refs/merge-requests/1/head"] {
        let err = git.fails(&src, &["push", &url, target]);
        assert!(err.contains("remote: slath:"), "no remote message: {err}");
        assert!(err.contains("reserved"), "{err}");
    }
    // The push is atomic: an allowed ref in the same push isn't written either.
    let err = git.fails(
        &src,
        &["push", &url, "HEAD:refs/heads/ok", "HEAD:refs/slath/y"],
    );
    assert!(err.contains("pre-receive hook declined"), "{err}");
    let refs = git.ok(
        &server.repo_path(&DEMO),
        &["for-each-ref", "--format=%(refname)"],
    );
    assert_eq!(refs, "", "rejected push left refs behind");
}

#[test]
fn conf_git_01_push_fails_closed_when_ipc_is_down() {
    let server = TestServer::start();
    let git = GitClient::new();
    let work = tempfile::tempdir().unwrap();
    let src = work.path().join("src");
    fixtures::generate("basic", &src, &[]);
    let url = server.url(&DEMO);
    git.ok(&src, &["push", "--quiet", &url, "main"]);
    let before = heads_and_tags(&git, &server.repo_path(&DEMO));

    // Take the IPC listener away: the hook can't reach the server.
    std::fs::remove_file(server.ipc_socket()).unwrap();
    let err = git.fails(&src, &["push", &url, "feature"]);
    assert!(err.contains("push rejected"), "{err}");
    assert_eq!(heads_and_tags(&git, &server.repo_path(&DEMO)), before);
    // Fetching doesn't need the hooks and keeps working.
    git.ok(work.path(), &["clone", "--quiet", &url, "c"]);
}

#[test]
fn conf_git_01_gzip_request_bodies_are_accepted() {
    let server = TestServer::start();
    let git = GitClient::new();
    let work = tempfile::tempdir().unwrap();
    let src = work.path().join("src");
    fixtures::generate("basic", &src, &[]);
    git.ok(&src, &["push", "--quiet", &server.url(&DEMO), "main"]);

    // A protocol v2 ls-refs request, gzip-compressed.
    let mut raw = Vec::new();
    raw.extend_from_slice(b"0014command=ls-refs\n");
    raw.extend_from_slice(b"0001");
    raw.extend_from_slice(b"0000");
    let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    gz.write_all(&raw).unwrap();
    let body = gz.finish().unwrap();
    let auth = base64_basic(&format!("git:{}", slath_tests::server::TOKEN));
    let mut req = format!(
        "POST /alice/demo.git/git-upload-pack HTTP/1.1\r\nHost: x\r\nAuthorization: Basic {auth}\r\n\
         Content-Type: application/x-git-upload-pack-request\r\nContent-Encoding: gzip\r\n\
         Git-Protocol: version=2\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    )
    .into_bytes();
    req.extend_from_slice(&body);
    let resp = String::from_utf8_lossy(&server.raw_request(&req)).into_owned();
    assert!(resp.starts_with("HTTP/1.1 200"), "{resp}");
    assert!(resp.contains("refs/heads/main"), "{resp}");
}

#[test]
fn credentials_and_dumb_http() {
    let server = TestServer::start();
    let get = |path: &str, auth: Option<&str>| {
        let auth = auth
            .map(|a| format!("Authorization: Basic {}\r\n", base64_basic(a)))
            .unwrap_or_default();
        let req = format!("GET {path} HTTP/1.1\r\nHost: x\r\n{auth}Connection: close\r\n\r\n");
        strip_date(&server.raw_request(req.as_bytes()))
    };
    let refs = "/info/refs?service=git-upload-pack";
    let token = format!("git:{}", slath_tests::server::TOKEN);
    // Missing or wrong credentials: byte-identical 401 for existing and
    // missing repositories.
    let existing = get(&format!("/alice/demo.git{refs}"), None);
    assert!(existing.starts_with("HTTP/1.1 401"), "{existing}");
    assert!(
        existing.contains("www-authenticate: Basic realm=\"Slath\""),
        "{existing}"
    );
    assert_eq!(existing, get(&format!("/alice/nope.git{refs}"), None));
    assert_eq!(
        existing,
        get(&format!("/alice/demo.git{refs}"), Some("git:wrong"))
    );
    assert_eq!(
        existing,
        get(
            &format!("/alice/demo.git{refs}"),
            Some(&format!(":{}", slath_tests::server::TOKEN))
        )
    );
    // Authenticated: 404 for missing repos; both URL forms resolve.
    assert!(get(&format!("/alice/nope.git{refs}"), Some(&token)).starts_with("HTTP/1.1 404"));
    let ok = get(&format!("/alice/demo{refs}"), Some(&token));
    assert!(ok.starts_with("HTTP/1.1 200"), "{ok}");
    assert!(ok.contains("content-type: application/x-git-upload-pack-advertisement"));
    assert!(ok.contains("cache-control: no-cache"));
    assert!(get(&format!("/ALICE/Demo.git{refs}"), Some(&token)).starts_with("HTTP/1.1 200"));
    // Dumb HTTP isn't served.
    assert!(get("/alice/demo.git/info/refs", Some(&token)).starts_with("HTTP/1.1 404"));
    assert!(get("/alice/demo.git/HEAD", Some(&token)).starts_with("HTTP/1.1 404"));
    assert!(get("/alice/demo.git/objects/info/packs", Some(&token)).starts_with("HTTP/1.1 404"));
}

fn strip_date(resp: &[u8]) -> String {
    String::from_utf8_lossy(resp)
        .lines()
        .filter(|l| !l.to_ascii_lowercase().starts_with("date:"))
        .collect::<Vec<_>>()
        .join("\n")
}

fn base64_basic(s: &str) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let b = s.as_bytes();
    let mut out = String::new();
    for chunk in b.chunks(3) {
        let n = (u32::from(chunk[0]) << 16)
            | (u32::from(*chunk.get(1).unwrap_or(&0)) << 8)
            | u32::from(*chunk.get(2).unwrap_or(&0));
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(T[((n >> (18 - 6 * i)) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}
