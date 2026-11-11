// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Regression tests derived from advisories in other forges and in Git.
//! Each test names the advisory it guards against.
//!
//! This file covers authorization: existence oracles, alternate
//! representations, token scopes and repository restrictions, IDOR on IDs,
//! and stale access after revocation.

use std::path::{Path, PathBuf};
use std::process::Output;

use slath_http::routes::{Access, route_table};
use slath_tests::fixtures;
use slath_tests::http::{Client, Resp, create_token, csrf_token, submit};
use slath_tests::server::{GitClient, PASSWORD, TestRepo, TestServer};
use slath_tests::sshd::{self, Sshd};

use crate::repos::normalized;

const PRIV: TestRepo = TestRepo {
    namespace: "alice",
    name: "priv",
};
const MPW: &str = "correct horse battery staple";

/// A server with alice's private `alice/priv` (basic fixture pushed) and
/// mallory, a signed-in stranger with a `repo:read` token and an SSH key.
struct World {
    // Field order: sshd goes before the server.
    sshd: Option<Sshd>,
    s: TestServer,
    git: GitClient,
    work: tempfile::TempDir,
    head: String,
    mallory: Client,
    mallory_token: String,
    mallory_key: PathBuf,
}

impl World {
    fn new(with_ssh: bool) -> Self {
        // The probes are many anonymous API requests; this is about who may
        // do what, not about rate limits.
        let s = TestServer::start_full(
            &[PRIV],
            "[limits]\napi_anonymous_requests_per_hour = 100000\n",
        );
        let git = GitClient::new();
        let work = tempfile::tempdir().unwrap();
        let src = work.path().join("src");
        fixtures::generate("basic", &src, &[]);
        git.ok(
            &src,
            &["push", "-q", &s.url(&PRIV), "refs/heads/*:refs/heads/*"],
        );
        let head = git.ok(&src, &["rev-parse", "main"]).trim().to_owned();
        s.create_user("mallory", MPW, false);
        let (mallory_key, public) = sshd::keygen(work.path(), "mallory");
        s.add_ssh_key_pw("mallory", MPW, &public);
        let mut mallory = s.login("mallory", MPW);
        let mallory_token = create_token(&mut mallory, "probe", &["repo:read"]);
        let sshd = (with_ssh && !sshd::skipped()).then(|| Sshd::start(&s));
        Self {
            sshd,
            s,
            git,
            work,
            head,
            mallory,
            mallory_token,
            mallory_key,
        }
    }

    fn base(&self) -> String {
        self.s.base()
    }

    fn authority(&self) -> String {
        self.base().trim_start_matches("http://").to_owned()
    }

    /// `git <args>` in the work directory, with mallory's SSH key.
    fn git_run(&self, args: &[&str]) -> Output {
        let mut c = self.git.cmd(self.work.path());
        if let Some(sshd) = &self.sshd {
            c.env("GIT_SSH_COMMAND", sshd.git_ssh_command(&self.mallory_key));
        }
        c.args(args).output().unwrap()
    }
}

trait AddKey {
    fn add_ssh_key_pw(&self, user: &str, password: &str, public: &str);
}

impl AddKey for TestServer {
    fn add_ssh_key_pw(&self, user: &str, password: &str, public: &str) {
        let mut c = self.login(user, password);
        let r = submit(
            &mut c,
            "/-/settings/keys",
            "/-/settings/keys",
            &[("title", "k"), ("key", public)],
        );
        assert_eq!(r.status, 303, "{}", r.body);
    }
}

/// Status and body with the twin's name replaced, nonces blanked.
fn same(private: &Resp, missing: &Resp, twin: &str, name: &str) -> bool {
    let mut m = Resp {
        status: missing.status,
        headers: missing.headers.clone(),
        body: missing.body.replace(twin, name),
        bytes: Vec::new(),
    };
    m.body = m
        .body
        .replace(&format!("{twin}.git"), &format!("{name}.git"));
    normalized(private) == normalized(&m)
}

/// Header names that a response sets, minus per-response values.
fn header_names(r: &Resp) -> Vec<String> {
    r.headers
        .keys()
        .filter(|k| *k != "content-length")
        .cloned()
        .collect()
}

fn api(
    c: &mut Client,
    method: &str,
    path: &str,
    token: Option<&str>,
    body: Option<serde_json::Value>,
) -> Resp {
    c.api(method, path, token, body.as_ref())
}

fn concrete(template: &str, repo: &str, head: &str) -> String {
    template
        .split('/')
        .map(|seg| match seg {
            "{ns}" => "alice".to_owned(),
            "{repo}" => repo.to_owned(),
            "{user}" => "alice".to_owned(),
            "{team}" => "devs".to_owned(),
            "{n}" | "{number}" => "1".to_owned(),
            "{position}" | "{pos}" => "0".to_owned(),
            "{id}" => "0192f1a2-3b4c-7d5e-8f60-718293a4b5c6".to_owned(),
            "{what}" => "labels".to_owned(),
            "{name}" => "bug".to_owned(),
            "{op}" => "archive".to_owned(),
            "{ref}" => "main".to_owned(),
            "{oid}" | "{rest}" => head.to_owned(),
            "{*rest}" if template.contains("/archive/") => "main.tar.gz".to_owned(),
            "{*rest}" => "main/README.md".to_owned(),
            s if s.starts_with('{') => panic!("no value for {s} in {template}"),
            s => s.to_owned(),
        })
        .collect::<Vec<_>>()
        .join("/")
}

/// Every repository route, as anonymous visitor, as a stranger with a
/// session cookie, and as a stranger with a token, answers for a private
/// repository exactly what it answers for one that doesn't exist.
///
/// Guards against Gitea's feed, badge, archive, and home-page bypasses
/// (GHSA-3pww-vcvm-3gmj, GHSA-r96c-jc75-rxv6, GHSA-cr4g-f395-h25h,
/// GHSA-cp3q-vrj2-ghhh).
#[test]
fn conf_rbac_01_route_enumeration_matrix() {
    let mut w = World::new(false);
    let token = w.mallory_token.clone();
    let mut failures = Vec::new();
    let repo_routes: Vec<_> = route_table()
        .into_iter()
        .filter(|(t, _)| t.contains("{repo}"))
        .collect();
    assert!(repo_routes.len() >= 25, "{repo_routes:?}");
    for (template, access) in repo_routes {
        assert!(
            matches!(access, Access::Resource | Access::Git),
            "{template}"
        );
        let private = concrete(template, "priv", &w.head);
        let missing = concrete(template, "doesnotexist", &w.head);
        for method in ["GET", "HEAD", "OPTIONS"] {
            for who in ["anonymous", "cookie", "token"] {
                let send = |w: &mut World, path: &str| -> Resp {
                    let bearer = format!("Bearer {token}");
                    match who {
                        "anonymous" => w.s.client().request(method, path, &[], None),
                        "cookie" => w.mallory.request(method, path, &[], None),
                        _ => {
                            let mut c = Client::new(&w.base());
                            c.request(method, path, &[("authorization", &bearer)], None)
                        }
                    }
                };
                let p = send(&mut w, &private);
                let m = send(&mut w, &missing);
                let ctx = format!("{who} {method} {private} -> {}", p.status);
                // Redirects are fine only when a missing repository gets the
                // same one (checked next).
                if (200..300).contains(&p.status) {
                    failures.push(format!("{ctx}: not refused"));
                }
                if !same(&p, &m, "doesnotexist", "priv") {
                    failures.push(format!("{ctx}: differs from missing ({})", m.status));
                }
                if header_names(&p) != header_names(&m) {
                    failures.push(format!("{ctx}: header names differ"));
                }
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// Alternate representations of a private repository (feeds, patches,
/// `go-get`, `Accept` variants, dumb-HTTP files) reveal nothing.
///
/// Guards against Gitea GHSA-3pww-vcvm-3gmj / GHSA-6cqf-375w-639g /
/// GHSA-hf55-9cwq-2x64 (RSS/Atom and `Accept:` feeds), GHSA-p4mj-98mv-xq26
/// (`?go-get=1`), and Forgejo CVE-2023-49948 (`.rss` existence oracle).
#[test]
fn conf_rbac_01_suffix_and_accept_variants() {
    let mut w = World::new(false);
    let head = w.head.clone();
    let paths = [
        "/alice/{r}.rss".to_owned(),
        "/alice/{r}.atom".to_owned(),
        "/alice/{r}.git".to_owned(),
        "/alice/{r}.json".to_owned(),
        format!("/alice/{{r}}/commit/{head}.patch"),
        format!("/alice/{{r}}/commit/{head}.diff"),
        "/alice/{r}?go-get=1".to_owned(),
        "/alice/{r}.git?go-get=1".to_owned(),
        "/alice/{r}/info/refs".to_owned(),
        "/alice/{r}.git/info/refs".to_owned(),
        "/alice/{r}.git/HEAD".to_owned(),
        "/alice/{r}.git/config".to_owned(),
        "/alice/{r}.git/objects/info/packs".to_owned(),
        "/alice/{r}.git/objects/info/alternates".to_owned(),
        format!("/alice/{{r}}.git/objects/{}/{}", &head[..2], &head[2..]),
        "/alice/{r}.git/refs/heads/main".to_owned(),
        "/alice/{r}.git/packed-refs".to_owned(),
        "/alice/{r}/raw/main/README.md?download=1".to_owned(),
        "/alice/{r}/archive/main.zip".to_owned(),
        "/alice/{r}/tree/main?format=json".to_owned(),
        "/api/v1/repos/alice/{r}".to_owned(),
        "/api/v1/repos/alice/{r}/blob/main?path=README.md".to_owned(),
    ];
    let accepts = [
        "text/html",
        "application/json",
        "application/rss+xml",
        "application/atom+xml",
        "application/x-git-upload-pack-advertisement",
        "*/*",
    ];
    let mut failures = Vec::new();
    for path in &paths {
        for accept in accepts {
            for who in ["anonymous", "cookie"] {
                let private = path.replace("{r}", "priv");
                let missing = path.replace("{r}", "doesnotexist");
                let headers = [("accept", accept)];
                let (p, m) = if who == "anonymous" {
                    let mut c = w.s.client();
                    (
                        c.request("GET", &private, &headers, None),
                        c.request("GET", &missing, &headers, None),
                    )
                } else {
                    (
                        w.mallory.request("GET", &private, &headers, None),
                        w.mallory.request("GET", &missing, &headers, None),
                    )
                };
                let ctx = format!("{who} GET {private} (Accept: {accept}) -> {}", p.status);
                if (200..400).contains(&p.status) {
                    failures.push(format!("{ctx}: not refused"));
                }
                if !same(&p, &m, "doesnotexist", "priv") {
                    failures.push(format!("{ctx}: differs from missing ({})", m.status));
                }
                // Pages echo the requested path; anything else of the
                // repository is a leak.
                let echoed = p.body.replace(&private, "").replace(&urlish(&private), "");
                if echoed.contains(&head) || echoed.contains("Basic fixture") {
                    failures.push(format!("{ctx}: leaks content"));
                }
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// Private and nonexistent repositories look the same on every surface:
/// web UI, API, smart HTTP, SSH, archive, raw, and `go-get`.
///
/// Guards against existence oracles such as Gitea GHSA-p4mj-98mv-xq26 and
/// GHSA-hch4-r2j8-xm72, and Forgejo CVE-2023-49948.
#[test]
fn conf_git_03_indistinguishable_404() {
    let mut w = World::new(true);
    let mut failures = Vec::new();
    let check = |what: &str, p: &Resp, m: &Resp, failures: &mut Vec<String>| {
        if !same(p, m, "nope", "priv") {
            failures.push(format!(
                "{what}: {} vs {}:\n{}\n---\n{}",
                p.status, m.status, p.body, m.body
            ));
        }
        if header_names(p) != header_names(m) {
            failures.push(format!(
                "{what}: header names {:?} vs {:?}",
                header_names(p),
                header_names(m)
            ));
        }
    };
    for suffix in [
        "",
        "/tree/main",
        "/raw/main/README.md",
        "/archive/main.tar.gz",
        "?go-get=1",
        "/commits/main",
    ] {
        let (p, m) = {
            let mut c = w.s.client();
            (
                c.get(&format!("/alice/priv{suffix}")),
                c.get(&format!("/alice/nope{suffix}")),
            )
        };
        assert_eq!(p.status, 404, "anonymous {suffix}");
        check(&format!("anonymous UI {suffix}"), &p, &m, &mut failures);
        let p = w.mallory.get(&format!("/alice/priv{suffix}"));
        let m = w.mallory.get(&format!("/alice/nope{suffix}"));
        check(&format!("stranger UI {suffix}"), &p, &m, &mut failures);
    }
    for suffix in ["", "/tree/main", "/branches", "/commits", "/tags"] {
        for token in [None, Some(w.mallory_token.clone())] {
            let mut c = Client::new(&w.base());
            let p = api(
                &mut c,
                "GET",
                &format!("/api/v1/repos/alice/priv{suffix}"),
                token.as_deref(),
                None,
            );
            let m = api(
                &mut c,
                "GET",
                &format!("/api/v1/repos/alice/nope{suffix}"),
                token.as_deref(),
                None,
            );
            assert_eq!(p.status, 404, "API {suffix}");
            check(
                &format!("API {suffix} token={}", token.is_some()),
                &p,
                &m,
                &mut failures,
            );
        }
    }
    // Smart HTTP: anonymous visitors get the same challenge for both;
    // a stranger's token the same refusal.
    for service in ["git-upload-pack", "git-receive-pack"] {
        let mut c = w.s.client();
        let p = c.get(&format!("/alice/priv.git/info/refs?service={service}"));
        let m = c.get(&format!("/alice/nope.git/info/refs?service={service}"));
        assert!(matches!(p.status, 401 | 404), "{service}: {}", p.status);
        check(
            &format!("anonymous smart HTTP {service}"),
            &p,
            &m,
            &mut failures,
        );
        let auth = format!("Bearer {}", w.mallory_token);
        let mut c = Client::new(&w.base());
        let p = c.get_with(
            &format!("/alice/priv.git/info/refs?service={service}"),
            &[("authorization", &auth)],
        );
        let m = c.get_with(
            &format!("/alice/nope.git/info/refs?service={service}"),
            &[("authorization", &auth)],
        );
        check(
            &format!("token smart HTTP {service}"),
            &p,
            &m,
            &mut failures,
        );
    }
    // Git clients see the same messages.
    let authority = w.authority();
    let url = |r: &str| {
        format!(
            "http://mallory:{}@{authority}/alice/{r}.git",
            w.mallory_token
        )
    };
    let p = w.git_run(&["ls-remote", &url("priv")]);
    let m = w.git_run(&["ls-remote", &url("nope")]);
    assert!(!p.status.success());
    let (pe, me) = (
        String::from_utf8_lossy(&p.stderr),
        String::from_utf8_lossy(&m.stderr),
    );
    if pe.replace("priv", "X") != me.replace("nope", "X") {
        failures.push(format!("git over HTTP:\n{pe}\n---\n{me}"));
    }
    if let Some(sshd) = &w.sshd {
        for verb in ["ls-remote", "clone"] {
            let mut args = vec![verb];
            if verb == "clone" {
                args.push("-q");
            }
            let pu = sshd.url("alice", "priv");
            let mu = sshd.url("alice", "nope");
            let p = w.git_run(&[args.as_slice(), &[pu.as_str(), "c1"]].concat());
            let m = w.git_run(&[args.as_slice(), &[mu.as_str(), "c2"]].concat());
            assert!(!p.status.success());
            let (pe, me) = (
                String::from_utf8_lossy(&p.stderr),
                String::from_utf8_lossy(&m.stderr),
            );
            let norm =
                |s: &str, name: &str| s.replace(name, "X").replace("c1", "C").replace("c2", "C");
            if norm(&pe, "priv") != norm(&me, "nope") {
                failures.push(format!("git over SSH {verb}:\n{pe}\n---\n{me}"));
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// An old name of a renamed private repository redirects only readers, on
/// every surface, and never reveals the new name.
///
/// Guards against redirect leaks of private repositories' new names (the
/// Gitea/GitLab redirect-table existence oracles in the catalogue, C9).
#[test]
fn conf_rbac_01_rename_redirect_requires_read() {
    let mut w = World::new(true);
    let mut alice = w.s.login("alice", PASSWORD);
    let r = submit(
        &mut alice,
        "/alice/priv/settings/general",
        "/alice/priv/settings/rename",
        &[("name", "hidden")],
    );
    assert_eq!(r.status, 303, "{}", r.body);
    // The owner is redirected, keeping the rest of the path.
    let r = alice.get("/alice/priv/tree/main");
    assert!(matches!(r.status, 301 | 308), "{}", r.status);
    assert_eq!(r.header("location"), Some("/alice/hidden/tree/main"));

    for suffix in [
        "",
        "/tree/main",
        "/raw/main/README.md",
        "/archive/main.tar.gz",
    ] {
        for who in ["anonymous", "stranger"] {
            let (p, m) = if who == "anonymous" {
                let mut c = w.s.client();
                (
                    c.get(&format!("/alice/priv{suffix}")),
                    c.get(&format!("/alice/nope{suffix}")),
                )
            } else {
                (
                    w.mallory.get(&format!("/alice/priv{suffix}")),
                    w.mallory.get(&format!("/alice/nope{suffix}")),
                )
            };
            assert_eq!(p.status, 404, "{who} {suffix}");
            assert_eq!(p.header("location"), None, "{who} {suffix}");
            assert!(!p.body.contains("alice/hidden"), "{who} {suffix}");
            assert!(same(&p, &m, "nope", "priv"), "{who} {suffix}");
        }
    }
    for token in [None, Some(w.mallory_token.as_str())] {
        let mut c = Client::new(&w.base());
        let r = api(&mut c, "GET", "/api/v1/repos/alice/priv", token, None);
        assert_eq!(r.status, 404);
        assert_eq!(r.header("location"), None);
        assert!(!r.body.contains("alice/hidden"));
    }
    let mut c = w.s.client();
    let r = c.get("/alice/priv.git/info/refs?service=git-upload-pack");
    assert_eq!(r.header("location"), None);
    assert!(!r.body.contains("alice/hidden"));
    let authority = w.authority();
    let out = w.git_run(&[
        "clone",
        "-q",
        &format!(
            "http://mallory:{}@{authority}/alice/priv.git",
            w.mallory_token
        ),
        "h",
    ]);
    assert!(!out.status.success());
    assert!(!String::from_utf8_lossy(&out.stderr).contains("hidden"));
    if let Some(sshd) = &w.sshd {
        let out = w.git_run(&["clone", "-q", &sshd.url("alice", "priv"), "s"]);
        assert!(!out.status.success());
        assert!(!String::from_utf8_lossy(&out.stderr).contains("hidden"));
    }
}

/// A read-only token can't push, even for a writer.
///
/// Guards against Gitea GHSA-cc8w-r4qh-3v65 (smart HTTP skipped token
/// scopes) and GHSA-cr4g-f395-h25h (archive ignored token scope).
#[test]
fn conf_rbac_02_pat_read_cannot_push() {
    let w = World::new(false);
    w.s.create_user("bob", MPW, false);
    let mut alice = w.s.login("alice", PASSWORD);
    let r = submit(
        &mut alice,
        "/alice/priv/settings/access",
        "/alice/priv/settings/access",
        &[("username", "bob"), ("role", "admin")],
    );
    assert_eq!(r.status, 303);
    let mut bob = w.s.login("bob", MPW);
    let read = create_token(&mut bob, "read", &["repo:read"]);
    let write = create_token(&mut bob, "write", &["repo:write"]);
    let authority = w.authority();
    let src = w.work.path().join("src");
    let url = |t: &str| format!("http://bob:{t}@{authority}/alice/priv.git");
    // Reading works, pushing doesn't.
    w.git.ok(w.work.path(), &["ls-remote", &url(&read)]);
    let out = w
        .git
        .cmd(&src)
        .args(["push", &url(&read), "main:refs/heads/by-read-token"])
        .output()
        .unwrap();
    assert!(
        !out.status.success(),
        "push with a repo:read token succeeded"
    );
    let mut c = w.s.client();
    let basic = |t: &str| format!("Basic {}", b64(format!("bob:{t}").as_bytes()));
    let r = c.get_with(
        "/alice/priv.git/info/refs?service=git-receive-pack",
        &[("authorization", &basic(&read))],
    );
    assert!(
        matches!(r.status, 401 | 403 | 404),
        "receive-pack advertisement: {}",
        r.status
    );
    let r = c.request(
        "POST",
        "/alice/priv.git/git-receive-pack",
        &[("authorization", &basic(&read))],
        Some(("application/x-git-receive-pack-request", b"0000".to_vec())),
    );
    assert!(
        matches!(r.status, 401 | 403 | 404),
        "receive-pack POST: {}",
        r.status
    );
    // API changes need repo:write as well.
    let r = api(
        &mut c,
        "PATCH",
        "/api/v1/repos/alice/priv",
        Some(&read),
        Some(serde_json::json!({"description": "x"})),
    );
    assert_eq!(r.status, 403, "{}", r.body);
    let r = api(
        &mut c,
        "PATCH",
        "/api/v1/repos/alice/priv",
        Some(&write),
        Some(serde_json::json!({"description": "x"})),
    );
    assert_eq!(r.status, 200, "{}", r.body);
    w.git.ok(
        &src,
        &["push", "-q", &url(&write), "main:refs/heads/by-write-token"],
    );
    assert!(
        !w.git
            .ok(&w.s.repo_path(&PRIV), &["for-each-ref"])
            .contains("by-read-token")
    );
}

/// A token restricted to one repository can't reach another one the user
/// can read, on any surface, and lists don't show it.
///
/// Guards against Gitea GHSA-cc8w-r4qh-3v65 / GHSA-cr4g-f395-h25h and
/// GitLab-style repository-restriction bypasses (catalogue C7).
/// Restricted tokens can't be created in the UI yet, so the restriction is
/// set directly on the token row.
#[test]
fn conf_rbac_02_pat_repo_restriction_all_surfaces() {
    let w = World::new(true);
    let mut alice = w.s.login("alice", PASSWORD);
    slath_tests::http::create_repo(&mut alice, "alice", "other", "private");
    let other = TestRepo {
        namespace: "alice",
        name: "other",
    };
    w.git.ok(
        &w.work.path().join("src"),
        &["push", "-q", &w.s.url(&other), "main:refs/heads/main"],
    );
    let token = create_token(&mut alice, "only-priv", &["repo:read"]);
    w.s.db.execute(&format!(
        "UPDATE access_tokens SET repo_ids = ARRAY[(SELECT id FROM repositories WHERE name = 'priv')] \
         WHERE token_prefix = '{}'",
        &token["slath_pat_".len().."slath_pat_".len() + 8]
    ));
    assert_eq!(
        w.s.db
            .query_one("SELECT count(*)::text FROM access_tokens WHERE repo_ids IS NOT NULL"),
        "1",
        "the restriction was applied"
    );
    let authority = w.authority();
    let url = |r: &str| format!("http://alice:{token}@{authority}/alice/{r}.git");
    w.git.ok(w.work.path(), &["ls-remote", &url("priv")]);
    let out = w
        .git
        .cmd(w.work.path())
        .args(["ls-remote", &url("other")])
        .output()
        .unwrap();
    assert!(
        !out.status.success(),
        "clone of the other repository succeeded"
    );
    let mut c = Client::new(&w.base());
    for suffix in [
        "",
        "/tree/main",
        "/branches",
        "/commits",
        "/tags",
        "/blob/main?path=README.md",
    ] {
        let r = api(
            &mut c,
            "GET",
            &format!("/api/v1/repos/alice/priv{suffix}"),
            Some(&token),
            None,
        );
        assert_eq!(r.status, 200, "allowed repository {suffix}: {}", r.body);
        let r = api(
            &mut c,
            "GET",
            &format!("/api/v1/repos/alice/other{suffix}"),
            Some(&token),
            None,
        );
        let m = api(
            &mut c,
            "GET",
            &format!("/api/v1/repos/alice/nope{suffix}"),
            Some(&token),
            None,
        );
        assert_eq!(r.status, 404, "other repository {suffix}");
        assert!(same(&r, &m, "nope", "other"), "{suffix}");
    }
    let list = api(&mut c, "GET", "/api/v1/repos", Some(&token), None);
    assert_eq!(list.status, 200);
    assert!(list.body.contains("\"priv\""), "{}", list.body);
    assert!(
        !list.body.contains("\"other\""),
        "the list shows the other repository: {}",
        list.body
    );
    // Tokens never work on web pages (raw, archive): treated as anonymous.
    let bearer = format!("Bearer {token}");
    for path in [
        "/alice/other/raw/main/README.md",
        "/alice/other/archive/main.tar.gz",
        "/alice/priv/raw/main/README.md",
    ] {
        let r = Client::new(&w.base()).get_with(path, &[("authorization", &bearer)]);
        assert_eq!(r.status, 404, "{path}");
    }
}

/// Scopes the OpenAPI document declares are the scopes the server enforces:
/// a token without the declared scope never succeeds (and never changes
/// anything), and a token with it is never refused for lack of scope.
///
/// Guards against per-route scope omissions, the dominant Gitea 2026 bug
/// family (GHSA-ww9q-7j5q-hrv5, GHSA-hxhc-246f-rvqc, GHSA-h56g-4qw7-2mxg,
/// GHSA-fq2p-5p22-8g6j, GHSA-wrr5-99h5-gq57, GHSA-7p4h-3gxq-x3h3,
/// GHSA-8629-vc8r-5p58) and Gitea CVE-2025-68941 (public-only tokens
/// reading private data).
#[test]
fn conf_rbac_02_pat_scopes_property_test() {
    let w = World::new(false);
    let spec: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../docs/api/openapi.json"),
        )
        .unwrap(),
    )
    .unwrap();
    let mut alice = w.s.login("alice", PASSWORD);
    let r = submit(
        &mut alice,
        "/-/orgs/new",
        "/-/orgs",
        &[("name", "acme"), ("display_name", "")],
    );
    assert_eq!(r.status, 303);
    let (_, public) = sshd::keygen(w.work.path(), "alice");
    let r = submit(
        &mut alice,
        "/-/settings/keys",
        "/-/settings/keys",
        &[("title", "k"), ("key", &public)],
    );
    assert_eq!(r.status, 303);
    let key_id = w.s.db.query_one("SELECT id::text FROM ssh_keys LIMIT 1");
    let scopes = [
        "repo:read",
        "repo:write",
        "issues:write",
        "mrs:write",
        "statuses:write",
        "user:read",
        "user:write",
    ];
    let tokens: Vec<(&str, String)> = scopes
        .iter()
        .map(|s| (*s, create_token(&mut alice, s, &[s])))
        .collect();
    let token_id =
        w.s.db
            .query_one("SELECT id::text FROM access_tokens ORDER BY created_at LIMIT 1");
    let implies = |have: &str, need: &str| {
        have == need
            || (have == "repo:write" && need == "repo:read")
            || (have == "user:write" && need == "user:read")
    };
    let fingerprint = || {
        w.s.db.query_one(
            "SELECT md5(coalesce((SELECT string_agg(x::text, ',' ORDER BY x::text) FROM repositories x), '') \
             || coalesce((SELECT string_agg(x::text, ',' ORDER BY x::text) FROM repo_collaborators x), '') \
             || coalesce((SELECT string_agg(x::text, ',' ORDER BY x::text) FROM organizations x), '') \
             || coalesce((SELECT string_agg(x::text, ',' ORDER BY x::text) FROM org_members x), '') \
             || coalesce((SELECT string_agg(x.id::text || x.revoked_at::text, ',' ORDER BY x.id) FROM access_tokens x), '') \
             || coalesce((SELECT string_agg(x::text, ',' ORDER BY x::text) FROM ssh_keys x), ''))",
        )
    };
    let before = fingerprint();
    let mut failures = Vec::new();
    let mut checked = 0;
    for (path, ops) in spec["paths"].as_object().unwrap() {
        for (method, op) in ops.as_object().unwrap() {
            let method = method.to_uppercase();
            let Some(security) = op["security"].as_array() else {
                continue; // public operation
            };
            let optional = security
                .iter()
                .any(|s| s.as_object().is_some_and(serde_json::Map::is_empty));
            let needed: Vec<&str> = security
                .iter()
                .filter_map(|s| s["token"].as_array())
                .flatten()
                .filter_map(|v| v.as_str())
                .collect();
            let concrete = path
                .replace("{ns}", "alice")
                .replace("{repo}", "priv")
                .replace("{org}", "acme")
                .replace("{user}", "alice")
                .replace("{team}", "devs")
                .replace("{number}", "1")
                .replace("{n}", "1")
                .replace("{position}", "0")
                .replace("{ref}", "main")
                .replace("{oid}", &w.head)
                .replace(
                    "{id}",
                    if path.contains("/keys/") {
                        &key_id
                    } else {
                        &token_id
                    },
                );
            // Valid bodies, so that only the scope can refuse the call.
            let role = if path.contains("/orgs/") {
                "member"
            } else {
                "read"
            };
            let body = (method != "GET" && method != "DELETE").then(|| {
                serde_json::json!({
                    "name": "probe", "namespace": "alice", "visibility": "private",
                    "description": "probe", "role": role, "title": "probe",
                    "key": public, "scopes": ["repo:read"], "display_name": "probe",
                })
            });
            for (scope, token) in &tokens {
                let sufficient = !needed.is_empty() && needed.iter().any(|n| implies(scope, n));
                let mut c = Client::new(&w.base());
                if sufficient {
                    // Only reads are replayed with a sufficient token.
                    if method == "GET" {
                        let r = api(&mut c, &method, &concrete, Some(token), None);
                        checked += 1;
                        if matches!(r.status, 401 | 403) {
                            failures.push(format!(
                                "{method} {path} with {scope}: refused ({})",
                                r.status
                            ));
                        }
                    }
                    continue;
                }
                let r = api(&mut c, &method, &concrete, Some(token), body.clone());
                checked += 1;
                let refused = matches!(r.status, 401 | 403 | 404);
                if optional && method == "GET" {
                    // Public data only: nothing of the private repository.
                    if r.status == 200
                        && (r.body.contains("\"priv\"") || r.body.contains("Basic fixture"))
                    {
                        failures.push(format!("{method} {path} with {scope}: private data"));
                    }
                } else if !refused {
                    failures.push(format!(
                        "{method} {path} with {scope}: {} {}",
                        r.status, r.body
                    ));
                }
            }
        }
    }
    assert!(checked > 50, "only {checked} calls");
    assert!(failures.is_empty(), "{}", failures.join("\n"));
    assert_eq!(
        fingerprint(),
        before,
        "insufficiently scoped calls changed state"
    );
}

/// Tokens are not credentials for web pages, and cookies are not
/// credentials for the API.
///
/// Guards against Gitea GHSA-cp3q-vrj2-ghhh (PAT accepted on the HTML repo
/// page without a scope check).
#[test]
fn conf_rbac_02_pat_not_accepted_on_ui_routes() {
    let w = World::new(false);
    let token = w.s.token.clone();
    let mut anon = w.s.client();
    let reference = anon.get("/alice/priv");
    assert_eq!(reference.status, 404);
    let basic_raw = format!("alice:{token}");
    let basic = format!("Basic {}", b64(basic_raw.as_bytes()));
    for auth in [format!("Bearer {token}"), format!("token {token}"), basic] {
        for path in [
            "/alice/priv",
            "/alice/priv/raw/main/README.md",
            "/alice/priv/archive/main.tar.gz",
            "/-/settings/profile",
            "/",
        ] {
            let r = Client::new(&w.base()).get_with(path, &[("authorization", &auth)]);
            assert!(
                matches!(r.status, 303 | 401 | 404) || (path == "/" && r.status == 200),
                "{auth} {path}: {}",
                r.status
            );
            assert!(!r.body.contains("Basic fixture"), "{path}");
            assert!(
                r.set_cookies()
                    .iter()
                    .all(|c| !c.contains("session=") || c.contains("session=;")),
                "{path}: a session cookie was issued"
            );
            if path == "/" {
                assert!(
                    !r.body.contains("action=\"/-/logout\""),
                    "signed in by a token"
                );
            }
        }
    }
    // A session cookie doesn't authenticate API calls.
    let mut alice = w.s.login("alice", PASSWORD);
    let cookie = alice
        .cookies
        .iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join("; ");
    let r = alice.request(
        "GET",
        "/api/v1/user",
        &[("cookie", &cookie), ("accept", "application/json")],
        None,
    );
    assert_eq!(r.status, 401, "{}", r.body);
    let r = alice.request(
        "GET",
        "/api/v1/repos/alice/priv",
        &[("cookie", &cookie)],
        None,
    );
    assert_eq!(r.status, 404);
}

/// `path` as it appears percent-encoded in a query string.
fn urlish(path: &str) -> String {
    path.replace('/', "%2F")
}

fn b64(b: &[u8]) -> String {
    const A: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in b.chunks(3) {
        let n = chunk
            .iter()
            .enumerate()
            .fold(0u32, |acc, (i, x)| acc | (u32::from(*x) << (16 - 8 * i)));
        for i in 0..4 {
            out.push(if i <= chunk.len() {
                A[((n >> (18 - 6 * i)) & 63) as usize] as char
            } else {
                '='
            });
        }
    }
    out
}

/// A token can't create tokens, so it can't escalate its scopes, extend
/// its lifetime, or shed a repository restriction.
///
/// Guards against Gitea GHSA-ww9q-7j5q-hrv5-style scope escalation via
/// token management endpoints (catalogue C7) and GitLab token-minting bugs.
#[test]
fn conf_rbac_02_pat_cannot_mint_stronger_token() {
    let w = World::new(false);
    let mut alice = w.s.login("alice", PASSWORD);
    let tokens = [
        create_token(&mut alice, "uw", &["user:write"]),
        create_token(&mut alice, "rw", &["repo:write", "user:write"]),
    ];
    let count = || w.s.db.query_one("SELECT count(*)::text FROM access_tokens");
    let before = count();
    for t in &tokens {
        for body in [
            serde_json::json!({"name": "x", "scopes": ["admin"]}),
            serde_json::json!({"name": "x", "scopes": ["repo:write", "user:write"]}),
            serde_json::json!({"name": "x", "scopes": ["user:read"], "expires_in_days": null}),
            serde_json::json!({"name": "x", "scopes": ["user:read"], "expires_at": null}),
        ] {
            let mut c = Client::new(&w.base());
            let r = api(&mut c, "POST", "/api/v1/user/tokens", Some(t), Some(body));
            assert!(!(200..300).contains(&r.status), "{}: {}", r.status, r.body);
            assert!(!r.body.contains("slath_pat_"), "{}", r.body);
        }
        // Nor through the web form with the token as credential.
        let r = Client::new(&w.base()).request(
            "POST",
            "/-/settings/tokens",
            &[("authorization", &format!("Bearer {t}"))],
            Some((
                "application/x-www-form-urlencoded",
                b"name=x&expires=0&scopes=admin".to_vec(),
            )),
        );
        assert!(!(200..300).contains(&r.status), "{}", r.status);
    }
    assert_eq!(count(), before);
}

/// IDs from another repository don't reach into it through a repository
/// the caller controls.
///
/// Guards against Forgejo CVE-2023-49946 (object's owning repository not
/// checked against the one whose permission was evaluated) and Gitea
/// GHSA-xv9x-fj9g-vj6h / GHSA-pgqf-926r-548m (cross-repository IDs). Labels,
/// comments, and attachments don't exist yet; commits, trees, blobs, and
/// collaborators are the IDs that do.
#[test]
fn conf_rbac_02_cross_repo_ids() {
    let w = World::new(false);
    w.s.create_user("bob", MPW, false);
    let mut alice = w.s.login("alice", PASSWORD);
    let r = submit(
        &mut alice,
        "/alice/priv/settings/access",
        "/alice/priv/settings/access",
        &[("username", "bob"), ("role", "write")],
    );
    assert_eq!(r.status, 303);
    let mut m = w.s.login("mallory", MPW);
    slath_tests::http::create_repo(&mut m, "mallory", "pub", "public");
    let mtoken = create_token(&mut m, "w", &["repo:write"]);
    // Mallory's own repository has some unrelated history.
    let src = w.work.path().join("msrc");
    std::fs::create_dir(&src).unwrap();
    w.git.ok(&src, &["init", "-q"]);
    std::fs::write(src.join("m.txt"), "m\n").unwrap();
    w.git.ok(&src, &["add", "."]);
    w.git.ok(&src, &["commit", "-q", "-m", "m"]);
    let authority = w.authority();
    w.git.ok(
        &src,
        &[
            "push",
            "-q",
            &format!("http://mallory:{mtoken}@{authority}/mallory/pub.git"),
            "HEAD:refs/heads/main",
        ],
    );

    let head = &w.head;
    let tree = w
        .git
        .ok(&w.s.repo_path(&PRIV), &["rev-parse", "main^{tree}"])
        .trim()
        .to_owned();
    let fake = "0123456789abcdef0123456789abcdef01234567";
    let mut c = Client::new(&w.base());
    for (template, what) in [
        ("/api/v1/repos/mallory/pub/commits/{}", "API commit"),
        ("/api/v1/repos/mallory/pub/tree/{}", "API tree"),
        (
            "/api/v1/repos/mallory/pub/blob/{}?path=README.md",
            "API blob",
        ),
        ("/api/v1/repos/mallory/pub/commits?ref={}", "API history"),
    ] {
        for id in [head.as_str(), tree.as_str()] {
            let r = api(
                &mut c,
                "GET",
                &template.replace("{}", id),
                Some(&mtoken),
                None,
            );
            let n = api(
                &mut c,
                "GET",
                &template.replace("{}", fake),
                Some(&mtoken),
                None,
            );
            assert!(
                !(200..300).contains(&r.status),
                "{what} {id}: {} {}",
                r.status,
                r.body
            );
            assert_eq!(
                r.status, n.status,
                "{what}: differs from a never-existing ID"
            );
            assert!(!r.body.contains("Basic fixture"));
        }
    }
    for template in [
        "/mallory/pub/commit/{}",
        "/mallory/pub/tree/{}",
        "/mallory/pub/raw/{}/README.md",
        "/mallory/pub/archive/{}.tar.gz",
    ] {
        let r = m.get(&template.replace("{}", head));
        let n = m.get(&template.replace("{}", fake));
        assert_eq!(r.status, 404, "{template}");
        assert!(same(&r, &n, fake, head), "{template}");
    }
    // Removing bob through mallory's repository doesn't touch alice's.
    let r = api(
        &mut c,
        "DELETE",
        "/api/v1/repos/mallory/pub/collaborators/bob",
        Some(&mtoken),
        None,
    );
    assert!(
        !(200..300).contains(&r.status) || r.status == 204,
        "{}",
        r.status
    );
    let r = submit(
        &mut m,
        "/mallory/pub/settings/access",
        "/mallory/pub/settings/access/bob/remove",
        &[],
    );
    assert_ne!(r.status, 500);
    assert_eq!(
        w.s.db
            .query_one("SELECT count(*)::text FROM repo_collaborators"),
        "1",
        "alice's collaborator is gone"
    );
}

/// Another user's key, token, and session IDs are "not found" for mallory,
/// exactly like IDs that never existed, and nothing changes.
///
/// Guards against IDOR on user-scoped objects (Forgejo CVE-2023-49946,
/// Gitea GHSA-96qg-qxxj-r8xq family).
#[test]
fn conf_rbac_02_user_scoped_ids() {
    let mut w = World::new(false);
    let mut alice = w.s.login("alice", PASSWORD);
    let (_, public) = sshd::keygen(w.work.path(), "alice-key");
    let r = submit(
        &mut alice,
        "/-/settings/keys",
        "/-/settings/keys",
        &[("title", "k"), ("key", &public)],
    );
    assert_eq!(r.status, 303);
    let alice_user = "(SELECT u.id FROM users u JOIN namespaces n ON n.id = u.namespace_id WHERE n.name = 'alice')";
    let key = w.s.db.query_one(&format!(
        "SELECT id::text FROM ssh_keys WHERE user_id = {alice_user}"
    ));
    let token = w.s.db.query_one(&format!(
        "SELECT id::text FROM access_tokens WHERE user_id = {alice_user} LIMIT 1"
    ));
    let session_page = alice.get("/-/settings/sessions").body;
    let session = session_page
        .split("/-/settings/sessions/")
        .nth(1)
        .and_then(|r| r.split('/').next())
        .unwrap()
        .to_owned();
    let fake = "0192f1a2-3b4c-7d5e-8f60-718293a4b5c6";
    let fake_session = "0".repeat(session.len());
    let state = || {
        w.s.db.query_one(
            "SELECT (SELECT count(*) FROM ssh_keys) || ' ' || (SELECT count(*) FROM access_tokens WHERE revoked_at IS NULL) \
             || ' ' || (SELECT count(*) FROM sessions)",
        )
    };
    let before = state();
    let mtoken = create_token(&mut w.mallory, "uw", &["user:write"]);
    let after_mallory_token = state();
    let mut c = Client::new(&w.base());
    for (path, twin) in [
        (
            format!("/api/v1/user/keys/{key}"),
            format!("/api/v1/user/keys/{fake}"),
        ),
        (
            format!("/api/v1/user/tokens/{token}"),
            format!("/api/v1/user/tokens/{fake}"),
        ),
    ] {
        for method in ["GET", "DELETE"] {
            let r = api(&mut c, method, &path, Some(&mtoken), None);
            let n = api(&mut c, method, &twin, Some(&mtoken), None);
            assert!(
                matches!(r.status, 404 | 405),
                "{method} {path}: {}",
                r.status
            );
            assert_eq!(normalized(&r), normalized(&n), "{method} {path}");
        }
    }
    let csrf = csrf_token(&w.mallory.get("/-/settings/profile").body).unwrap();
    for (path, twin) in [
        (
            format!("/-/settings/keys/{key}/delete"),
            format!("/-/settings/keys/{fake}/delete"),
        ),
        (
            format!("/-/settings/tokens/{token}/revoke"),
            format!("/-/settings/tokens/{fake}/revoke"),
        ),
        (
            format!("/-/settings/sessions/{session}/revoke"),
            format!("/-/settings/sessions/{fake_session}/revoke"),
        ),
    ] {
        let r = w.mallory.post_form(&path, &[("_csrf", &csrf)]);
        let n = w.mallory.post_form(&twin, &[("_csrf", &csrf)]);
        assert!(
            !(200..400).contains(&r.status) || r.status == n.status,
            "{path}: {}",
            r.status
        );
        let (rs, ns) = (normalized(&r), normalized(&n));
        assert_eq!(rs.0, ns.0, "{path}");
    }
    assert_eq!(
        state(),
        after_mallory_token,
        "alice's keys, tokens, or sessions changed"
    );
    assert_ne!(before, after_mallory_token);
    // Alice is still signed in.
    assert!(alice.get("/").body.contains("action=\"/-/logout\""));
}

/// Read access on every surface for a user, given their credentials.
fn surfaces(
    w: &World,
    repo: &str,
    user: &str,
    web: &mut Client,
    token: &str,
    key: Option<&Path>,
) -> Vec<(&'static str, bool)> {
    let authority = w.authority();
    let base = w.base();
    let mut out = vec![
        ("web", web.get(&format!("/alice/{repo}")).status == 200),
        (
            "raw",
            web.get(&format!("/alice/{repo}/raw/main/README.md")).status == 200,
        ),
        (
            "archive",
            web.get(&format!("/alice/{repo}/archive/main.tar.gz"))
                .status
                == 200,
        ),
        (
            "api",
            Client::new(&base)
                .api(
                    "GET",
                    &format!("/api/v1/repos/alice/{repo}/tree/main"),
                    Some(token),
                    None,
                )
                .status
                == 200,
        ),
        (
            "api list",
            Client::new(&base)
                .api("GET", "/api/v1/repos", Some(token), None)
                .body
                .contains(&format!("\"{repo}\"")),
        ),
        (
            "http",
            w.git
                .cmd(w.work.path())
                .args([
                    "ls-remote",
                    &format!("http://{user}:{token}@{authority}/alice/{repo}.git"),
                ])
                .output()
                .unwrap()
                .status
                .success(),
        ),
    ];
    if let (Some(sshd), Some(key)) = (&w.sshd, key) {
        let ok = w
            .git
            .cmd(w.work.path())
            .env("GIT_SSH_COMMAND", sshd.git_ssh_command(key))
            .args(["ls-remote", &sshd.url("alice", repo)])
            .output()
            .unwrap()
            .status
            .success();
        out.push(("ssh", ok));
    }
    out
}

/// After a collaborator is removed, their very next request fails on every
/// surface, whatever credential they use.
///
/// Guards against Gitea GHSA-96gv-2xc4-4hxj / GHSA-j2w3-9c3r-g83q /
/// GHSA-44qc-pgvp-wx7v / GHSA-qf2f-qh6p-7v89 (stale access after
/// revocation) and GHSA-x8c3-3rp8-2j46 (demotion not applied).
#[test]
fn conf_rbac_01_revocation_effective_everywhere() {
    let w = World::new(true);
    w.s.create_user("bob", MPW, false);
    let (bob_key, public) = sshd::keygen(w.work.path(), "bob");
    w.s.add_ssh_key_pw("bob", MPW, &public);
    let mut alice = w.s.login("alice", PASSWORD);
    let r = submit(
        &mut alice,
        "/alice/priv/settings/access",
        "/alice/priv/settings/access",
        &[("username", "bob"), ("role", "write")],
    );
    assert_eq!(r.status, 303);
    let mut bob = w.s.login("bob", MPW);
    let token = create_token(&mut bob, "t", &["repo:write"]);
    let before = surfaces(&w, "priv", "bob", &mut bob, &token, Some(&bob_key));
    assert!(
        before.iter().all(|(_, ok)| *ok),
        "bob can't read before removal: {before:?}"
    );
    // Demotion to read: pushing stops at once.
    let r = submit(
        &mut alice,
        "/alice/priv/settings/access",
        "/alice/priv/settings/access",
        &[("username", "bob"), ("role", "read")],
    );
    assert_eq!(r.status, 303);
    let authority = w.authority();
    let out = w
        .git
        .cmd(&w.work.path().join("src"))
        .args([
            "push",
            &format!("http://bob:{token}@{authority}/alice/priv.git"),
            "main:refs/heads/bob",
        ])
        .output()
        .unwrap();
    assert!(!out.status.success(), "push after demotion");
    let r = submit(
        &mut alice,
        "/alice/priv/settings/access",
        "/alice/priv/settings/access/bob/remove",
        &[],
    );
    assert_eq!(r.status, 303);
    let after = surfaces(&w, "priv", "bob", &mut bob, &token, Some(&bob_key));
    let leaks: Vec<_> = after.iter().filter(|(_, ok)| *ok).collect();
    assert!(leaks.is_empty(), "bob still reads via {leaks:?}");
}

/// Making a public repository private takes it away from everyone else at
/// once: pages, raw, archives, API, lists, profile, Git over HTTP and SSH.
///
/// Guards against Gitea GHSA-wrf9-r3h7-7x5v (fork sync after the parent
/// became private) and GitHub CVE-2024-6336 (data exposed after a
/// visibility change).
#[test]
fn conf_rbac_01_public_to_private_flip() {
    let mut w = World::new(true);
    let mut alice = w.s.login("alice", PASSWORD);
    let r = submit(
        &mut alice,
        "/alice/priv/settings/general",
        "/alice/priv/settings/visibility",
        &[("visibility", "public")],
    );
    assert_eq!(r.status, 303, "{}", r.body);
    let token = w.mallory_token.clone();
    let key = w.mallory_key.clone();
    let mut web = w.s.login("mallory", MPW);
    let before = surfaces(&w, "priv", "mallory", &mut web, &token, Some(&key));
    assert!(
        before.iter().all(|(_, ok)| *ok),
        "public repository not readable: {before:?}"
    );
    assert!(w.mallory.get("/alice").body.contains("/alice/priv"));
    w.git.ok(
        w.work.path(),
        &[
            "clone",
            "-q",
            &format!("{}/alice/priv.git", w.base()),
            "mclone",
        ],
    );

    let r = submit(
        &mut alice,
        "/alice/priv/settings/general",
        "/alice/priv/settings/visibility",
        &[("visibility", "private")],
    );
    assert_eq!(r.status, 303, "{}", r.body);
    let after = surfaces(&w, "priv", "mallory", &mut web, &token, Some(&key));
    let leaks: Vec<_> = after.iter().filter(|(_, ok)| *ok).collect();
    assert!(leaks.is_empty(), "still readable via {leaks:?}");
    assert!(
        !w.mallory.get("/alice").body.contains("/alice/priv"),
        "profile lists it"
    );
    assert!(
        !w.s.client().get("/").body.contains("alice/priv"),
        "start page lists it"
    );
    let out = w
        .git
        .cmd(&w.work.path().join("mclone"))
        .args(["fetch", "origin"])
        .output()
        .unwrap();
    assert!(!out.status.success(), "an existing clone can still fetch");
}

/// Losing the role that granted access drops it everywhere, including
/// under the repository's old name after a rename.
///
/// Guards against Gitea GHSA-xfqq-h54g-92f8 (transfer leaves persistent
/// read access) and GHSA-h62v-wmrr-5qjq. Repository transfer doesn't exist
/// yet; the closest equivalent is access through an organization: carol
/// reads `acme/tools` as an org member, the repository is renamed, and
/// carol is removed from the org.
#[test]
fn conf_rbac_01_transfer_drops_old_access() {
    let w = World::new(true);
    w.s.create_user("carol", MPW, false);
    let (carol_key, public) = sshd::keygen(w.work.path(), "carol");
    w.s.add_ssh_key_pw("carol", MPW, &public);
    let mut alice = w.s.login("alice", PASSWORD);
    let r = submit(
        &mut alice,
        "/-/orgs/new",
        "/-/orgs",
        &[("name", "acme"), ("display_name", "")],
    );
    assert_eq!(r.status, 303);
    slath_tests::http::create_repo(&mut alice, "acme", "tools", "private");
    let tools = TestRepo {
        namespace: "acme",
        name: "tools",
    };
    w.git.ok(
        &w.work.path().join("src"),
        &["push", "-q", &w.s.url(&tools), "main:refs/heads/main"],
    );
    // Members read the org's repositories (not settable in the UI yet).
    w.s.db
        .execute("UPDATE organizations SET default_member_role = 'read'");
    let r = submit(
        &mut alice,
        "/acme/-/members",
        "/acme/-/members",
        &[("username", "carol"), ("role", "member")],
    );
    assert_eq!(r.status, 303);
    let mut carol = w.s.login("carol", MPW);
    let token = create_token(&mut carol, "t", &["repo:read"]);
    let authority = w.authority();
    let reads = |carol: &mut Client, name: &str| -> Vec<(&'static str, bool)> {
        let mut out = vec![
            ("web", carol.get(&format!("/acme/{name}")).status == 200),
            (
                "raw",
                carol
                    .get(&format!("/acme/{name}/raw/main/README.md"))
                    .status
                    == 200,
            ),
            (
                "api",
                Client::new(&w.base())
                    .api(
                        "GET",
                        &format!("/api/v1/repos/acme/{name}/tree/main"),
                        Some(&token),
                        None,
                    )
                    .status
                    == 200,
            ),
            (
                "http",
                w.git
                    .cmd(w.work.path())
                    .args([
                        "ls-remote",
                        &format!("http://carol:{token}@{authority}/acme/{name}.git"),
                    ])
                    .output()
                    .unwrap()
                    .status
                    .success(),
            ),
        ];
        if let Some(sshd) = &w.sshd {
            let ok = w
                .git
                .cmd(w.work.path())
                .env("GIT_SSH_COMMAND", sshd.git_ssh_command(&carol_key))
                .args(["ls-remote", &sshd.url("acme", name)])
                .output()
                .unwrap()
                .status
                .success();
            out.push(("ssh", ok));
        }
        out
    };
    let before = reads(&mut carol, "tools");
    assert!(before.iter().all(|(_, ok)| *ok), "{before:?}");
    let r = submit(
        &mut alice,
        "/acme/tools/settings/general",
        "/acme/tools/settings/rename",
        &[("name", "gear")],
    );
    assert_eq!(r.status, 303, "{}", r.body);
    let r = submit(
        &mut alice,
        "/acme/-/members",
        "/acme/-/members/carol/remove",
        &[],
    );
    assert_eq!(r.status, 303);
    for name in ["tools", "gear"] {
        let after = reads(&mut carol, name);
        let leaks: Vec<_> = after.iter().filter(|(_, ok)| *ok).collect();
        assert!(leaks.is_empty(), "{name}: still readable via {leaks:?}");
        let r = carol.get(&format!("/acme/{name}"));
        assert_eq!(r.header("location"), None, "{name}");
    }
}
