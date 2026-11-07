// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Importing repositories (GIT-06, SEC-04): all branches, tags, and
//! objects arrive; unsupported schemes, internal addresses, and option
//! injection are refused before Git runs; redirects aren't followed; and
//! credentials don't stay behind.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use slath_tests::fixtures;
use slath_tests::http::{Client, create_token};
use slath_tests::server::{DEMO, GitClient, PASSWORD, TestServer};

const ALLOW_LOCAL: &str = "[import]\nallow_http = true\nallow_networks = [\"127.0.0.0/8\"]\n";

/// A source server with a private repository holding fixture `name`.
fn source(name: &str) -> (TestServer, tempfile::TempDir) {
    let s = TestServer::start();
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
    (s, d)
}

fn import(c: &mut Client, token: &str, name: &str, url: &str) -> slath_tests::http::Resp {
    import_with(c, token, name, url, "someone", "s3cret-import-pw")
}

fn import_with(
    c: &mut Client,
    token: &str,
    name: &str,
    url: &str,
    username: &str,
    password: &str,
) -> slath_tests::http::Resp {
    c.api(
        "POST",
        "/api/v1/repos/import",
        Some(token),
        Some(&serde_json::json!({
            "namespace": "alice", "name": name, "url": url,
            "username": username, "password": password,
        })),
    )
}

fn wait_done(c: &mut Client, token: &str, name: &str) -> serde_json::Value {
    let start = std::time::Instant::now();
    loop {
        let r = c.api(
            "GET",
            &format!("/api/v1/repos/alice/{name}/import"),
            Some(token),
            None,
        );
        let j = r.json();
        if j["state"] == "done" || j["state"] == "failed" {
            return j;
        }
        assert!(start.elapsed() < Duration::from_secs(60), "{j}");
        std::thread::sleep(Duration::from_millis(200));
    }
}

fn refs(git_dir: &std::path::Path) -> String {
    GitClient::new().ok(
        git_dir,
        &[
            "for-each-ref",
            "--format=%(refname) %(objectname)",
            "refs/heads",
            "refs/tags",
        ],
    )
}

#[test]
fn conf_git_06_import_copies_every_branch_tag_and_object() {
    let (src, _d) = source("tags");
    let mut src_alice = src.login("alice", PASSWORD);
    let src_token = create_token(&mut src_alice, "export", &["repo:read"]);
    let s = TestServer::start_full(&[], ALLOW_LOCAL);
    s.create_user("alice", PASSWORD, false);
    let mut a = s.login("alice", PASSWORD);
    let token = create_token(&mut a, "t", &["repo:write"]);
    let url = format!("{}/alice/demo.git", src.base());
    // Without credentials the private source refuses.
    let r = import_with(&mut a, &token, "nocreds", &url, "", "");
    assert_eq!(r.status, 202, "{}", r.body);
    assert_eq!(wait_done(&mut a, &token, "nocreds")["state"], "failed");
    let r = import_with(&mut a, &token, "copy", &url, "alice", &src_token);
    assert_eq!(r.status, 202, "{}", r.body);
    assert_eq!(r.json()["url"], url);
    let report = wait_done(&mut a, &token, "copy");
    assert_eq!(report["state"], "done", "{report}\n{}", s.log());

    let original = refs(&src.repo_path(&DEMO));
    let copy_path = s.repo_path(&slath_tests::server::TestRepo {
        namespace: "alice",
        name: "copy",
    });
    assert_eq!(refs(&copy_path), original);
    GitClient::new().ok(&copy_path, &["fsck", "--strict", "--no-dangling"]);
    let branches = original
        .lines()
        .filter(|l| l.starts_with("refs/heads/"))
        .count();
    let tags = original
        .lines()
        .filter(|l| l.starts_with("refs/tags/"))
        .count();
    assert_eq!(report["branches"], branches);
    assert_eq!(report["tags"], tags);
    assert!(tags > 0);
    // The database knows the refs, and the repository serves clones.
    let page = a.get("/alice/copy/branches");
    assert_eq!(page.status, 200);
    let read = create_token(&mut a, "read", &["repo:read"]);
    let clone_url = format!(
        "{}/alice/copy.git",
        s.base().replacen("://", &format!("://alice:{read}@"), 1)
    );
    let work = tempfile::tempdir().unwrap();
    GitClient::new().ok(work.path(), &["clone", "-q", &clone_url, "c"]);

    // The credentials were used once and are gone.
    assert_eq!(
        s.db.query_one("SELECT count(*)::text FROM jobs WHERE payload ? 'credentials' OR payload::text LIKE '%slath_pat_%'"),
        "0"
    );
    let tmp = s.data_dir().join("tmp");
    let leftovers = std::fs::read_dir(&tmp)
        .map(|d| {
            d.flatten()
                .filter(|e| e.file_name().to_string_lossy().starts_with("import-"))
                .count()
        })
        .unwrap_or(0);
    assert_eq!(leftovers, 0);
}

#[test]
fn conf_git_06_bad_urls_are_refused_before_git_runs() {
    let s = TestServer::start();
    let mut a = s.login("alice", PASSWORD);
    let token = create_token(&mut a, "t", &["repo:write"]);
    for (i, url) in [
        "file:///etc/passwd",
        "ssh://example.org/x.git",
        "git://example.org/x.git",
        "ext::sh -c touch% /tmp/x",
        "--upload-pack=touch /tmp/x",
        "https://127.0.0.1/x.git",
        "https://[::1]/x.git",
        "https://169.254.169.254/latest/meta-data",
        "https://10.0.0.1/x.git",
        "https://localhost/x.git",
        "http://example.org/x.git",
        "https://user:pw@example.org/x.git",
        "https://h/x\n[core]\nsshCommand=touch /tmp/x",
    ]
    .iter()
    .enumerate()
    {
        let r = import(&mut a, &token, &format!("r{i}"), url);
        assert_eq!(r.status, 422, "{url}: {}", r.body);
        assert!(r.body.contains("\"url\""), "{url}: {}", r.body);
    }
    // Nothing was created.
    assert_eq!(
        s.db.query_one("SELECT count(*)::text FROM repo_imports"),
        "0"
    );
    assert_eq!(
        s.db.query_one("SELECT count(*)::text FROM repositories WHERE name LIKE 'r%'"),
        "0"
    );
    // A token without repo:write can't import.
    let narrow = create_token(&mut a, "n", &["repo:read"]);
    assert_eq!(
        import(&mut a, &narrow, "x", "https://example.org/x.git").status,
        403
    );
}

/// An HTTP server that answers everything with a redirect to `to`.
fn redirector(to: String) -> (u16, Arc<AtomicUsize>) {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    let hits = Arc::new(AtomicUsize::new(0));
    let h = Arc::clone(&hits);
    std::thread::spawn(move || {
        for mut c in l.incoming().flatten() {
            h.fetch_add(1, Ordering::SeqCst);
            let mut buf = [0u8; 4096];
            let _ = c.read(&mut buf);
            let _ = write!(
                c,
                "HTTP/1.1 301 Moved Permanently\r\nLocation: {to}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            );
        }
    });
    (port, hits)
}

/// An HTTP server that only counts connections.
fn counter() -> (u16, Arc<AtomicUsize>) {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    let hits = Arc::new(AtomicUsize::new(0));
    let h = Arc::clone(&hits);
    std::thread::spawn(move || {
        for c in l.incoming().flatten() {
            h.fetch_add(1, Ordering::SeqCst);
            drop(c);
        }
    });
    (port, hits)
}

#[test]
fn conf_git_06_redirects_are_not_followed() {
    let (internal, internal_hits) = counter();
    let (port, hits) = redirector(format!(
        "http://127.0.0.1:{internal}/x.git/info/refs?service=git-upload-pack"
    ));
    let s = TestServer::start_full(&[], ALLOW_LOCAL);
    s.create_user("alice", PASSWORD, false);
    let mut a = s.login("alice", PASSWORD);
    let token = create_token(&mut a, "t", &["repo:write"]);
    let r = import(
        &mut a,
        &token,
        "redir",
        &format!("http://127.0.0.1:{port}/x.git"),
    );
    assert_eq!(r.status, 202, "{}", r.body);
    let report = wait_done(&mut a, &token, "redir");
    assert_eq!(report["state"], "failed", "{report}");
    assert!(hits.load(Ordering::SeqCst) >= 1);
    assert_eq!(
        internal_hits.load(Ordering::SeqCst),
        0,
        "the redirect was followed"
    );
    // The owner sees why on the repository page.
    let page = a.get("/alice/redir");
    assert!(page.body.contains("failed"), "{}", page.body);
}

/// Imported history is scanned for secrets but not rejected: it exists
/// elsewhere already (SEC-08, report-only).
#[test]
fn conf_sec_08_imports_report_secrets_without_rejecting() {
    let (src, d) = source("basic");
    // The source accepted the secret (push protection off there).
    src.db
        .execute("UPDATE repositories SET push_protection = 'off'");
    let git = GitClient::new();
    let w = d.path().join("src");
    let (_, secret) = slath_core::secret_scan::samples()
        .into_iter()
        .find(|(r, _)| *r == "stripe-secret-key")
        .unwrap();
    std::fs::write(w.join("payments.env"), secret).unwrap();
    git.ok(&w, &["add", "."]);
    git.ok(&w, &["commit", "-q", "-m", "payments"]);
    git.ok(&w, &["push", "-q", &src.url(&DEMO), "HEAD"]);
    let mut src_alice = src.login("alice", PASSWORD);
    let src_token = create_token(&mut src_alice, "export", &["repo:read"]);

    let s = TestServer::start_full(&[], ALLOW_LOCAL);
    s.create_user("alice", PASSWORD, false);
    let mut a = s.login("alice", PASSWORD);
    let token = create_token(&mut a, "t", &["repo:write"]);
    let url = format!("{}/alice/demo.git", src.base());
    let r = import_with(&mut a, &token, "copy", &url, "alice", &src_token);
    assert_eq!(r.status, 202, "{}", r.body);
    let report = wait_done(&mut a, &token, "copy");
    assert_eq!(report["state"], "done", "{report}");
    assert_eq!(report["secret_findings"], 1, "{report}");
    let page = a.get("/alice/copy");
    assert!(
        page.body.contains("found 1 possible secret(s)"),
        "{}",
        page.body
    );
}
