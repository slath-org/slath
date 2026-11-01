// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Route enumeration: every route of the server is registered with an
//! access marker, and each marker's promise holds for anonymous visitors
//! and for a signed-in stranger, with every method.

use slath_http::routes::{Access, route_table};
use slath_tests::fixtures;
use slath_tests::http::{Client, Resp, create_token, csrf_token, submit};
use slath_tests::server::{GitClient, TestRepo, TestServer};

use crate::repos::normalized;

const PW: &str = "correct horse battery staple";
const SECRET: TestRepo = TestRepo {
    namespace: "alice",
    name: "secret",
};

/// No source file registers a route without a marker. (Clippy forbids
/// `Router::route` too; this also catches the macro-free spellings.)
#[test]
fn conf_sec_01_every_route_has_an_access_marker() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../crates/http/src");
    let mut stack = vec![root];
    let mut offenders = Vec::new();
    while let Some(dir) = stack.pop() {
        for e in std::fs::read_dir(&dir).unwrap() {
            let p = e.unwrap().path();
            if p.is_dir() {
                stack.push(p);
            } else if p.extension().is_some_and(|x| x == "rs") && !p.ends_with("routes.rs") {
                let text = std::fs::read_to_string(&p).unwrap();
                for needle in [".route(", ".route_service(", ".nest(", ".nest_service("] {
                    if text.contains(needle) {
                        offenders.push(format!("{}: {needle}", p.display()));
                    }
                }
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "routes without an access marker:\n{}",
        offenders.join("\n")
    );
    let table = route_table();
    assert!(table.len() >= 70, "only {} routes registered", table.len());
}

/// Fills a route template for the private target (or the missing twin).
fn concrete(template: &str, missing: bool, head: &str) -> String {
    let mut out = Vec::new();
    for seg in template.split('/') {
        let v = match seg {
            "{ns}" => "alice".to_owned(),
            "{repo}" => if missing { "nothere" } else { "secret" }.to_owned(),
            "{org}" => if missing { "noorg" } else { "acme" }.to_owned(),
            "{name}" | "{user}" => "alice".to_owned(),
            "{team}" => "devs".to_owned(),
            "{op}" if template.contains("/settings/state/") => "archive".to_owned(),
            "{op}" => "suspend".to_owned(),
            "{id}" => "0192f1a2-3b4c-7d5e-8f60-718293a4b5c6".to_owned(),
            "{file}" => "alice.svg".to_owned(),
            "{hash}" => "0".to_owned(),
            "{ref}" => "main".to_owned(),
            "{oid}" | "{rest}" => head.to_owned(),
            "{*rest}" if template.contains("/archive/") => "main.tar.gz".to_owned(),
            "{*rest}" if template.starts_with("/api/") && !template.contains("/repos/") => {
                "v1/nothing".to_owned()
            }
            "{*rest}" => "main/README.md".to_owned(),
            "{*file}" => "app.js".to_owned(),
            s if s.starts_with('{') => panic!("no value for {s} in {template}"),
            s => s.to_owned(),
        };
        out.push(v);
    }
    out.join("/")
}

struct Who {
    name: &'static str,
    client: Client,
    csrf: Option<String>,
    token: Option<String>,
}

impl Who {
    fn send(&mut self, method: &str, path: &str) -> Resp {
        let api = path.starts_with("/api/");
        if api {
            let auth = self.token.as_ref().map(|t| format!("Bearer {t}"));
            let mut headers = vec![("accept", "application/json")];
            if let Some(a) = &auth {
                headers.push(("authorization", a.as_str()));
            }
            let body = (method != "GET").then(|| ("application/json", b"{}".to_vec()));
            // API requests carry no cookies.
            let mut c = Client::new(&self.client.base.clone());
            return c.request(method, path, &headers, body);
        }
        if method == "GET" {
            return self.client.get(path);
        }
        let csrf = self.csrf.clone().unwrap_or_default();
        let body = slath_tests::http::form_encode(&[
            ("_csrf", &csrf),
            ("confirm", "alice/secret"),
            ("name", "x"),
            ("role", "admin"),
            ("username", "mallory"),
            ("visibility", "public"),
        ]);
        self.client.request(
            method,
            path,
            &[],
            Some(("application/x-www-form-urlencoded", body.into_bytes())),
        )
    }
}

/// Everything of alice's and acme's that a stranger's request could change.
fn fingerprint(s: &TestServer, git: &GitClient) -> String {
    let alice = "(SELECT id FROM namespaces WHERE name = 'alice')";
    let acme = "(SELECT id FROM namespaces WHERE name = 'acme')";
    let alice_user = format!("(SELECT id FROM users WHERE namespace_id = {alice})");
    let repos = format!("(SELECT id FROM repositories WHERE namespace_id = {alice})");
    let acme_org = format!("(SELECT id FROM organizations WHERE namespace_id = {acme})");
    let queries = [
        format!("SELECT * FROM namespaces WHERE id IN ({alice}, {acme})"),
        format!("SELECT * FROM users WHERE namespace_id = {alice}"),
        format!("SELECT * FROM organizations WHERE namespace_id = {acme}"),
        format!("SELECT * FROM org_members WHERE org_id = {acme_org}"),
        format!("SELECT * FROM repositories WHERE namespace_id = {alice}"),
        format!("SELECT * FROM repo_collaborators WHERE repo_id IN {repos}"),
        format!("SELECT * FROM repo_redirects WHERE repo_id IN {repos}"),
        format!("SELECT * FROM git_refs WHERE repo_id IN {repos}"),
        format!("SELECT * FROM access_tokens WHERE user_id = {alice_user}"),
        format!("SELECT * FROM ssh_keys WHERE user_id = {alice_user}"),
    ];
    let mut out = String::new();
    for q in queries {
        out += &s.db.query_one(&format!(
            "SELECT coalesce(md5(string_agg(x::text, ',' ORDER BY x::text)), '-') FROM ({q}) x"
        ));
        out.push(' ');
    }
    out + &git.ok(&s.repo_path(&SECRET), &["for-each-ref"])
}

#[test]
fn conf_rbac_01_route_markers_hold_for_strangers_and_anonymous_visitors() {
    let s = TestServer::start_with(&[SECRET]);
    s.create_user("mallory", PW, false);
    let git = GitClient::new();
    let work = tempfile::tempdir().unwrap();
    let src = work.path().join("src");
    fixtures::generate("basic", &src, &[]);
    git.ok(
        &src,
        &["push", "-q", &s.url(&SECRET), "refs/heads/*:refs/heads/*"],
    );
    let head = git.ok(&src, &["rev-parse", "main"]).trim().to_owned();
    let mut alice = s.login("alice", slath_tests::server::PASSWORD);
    let r = submit(
        &mut alice,
        "/-/orgs/new",
        "/-/orgs",
        &[("name", "acme"), ("display_name", "")],
    );
    assert_eq!(r.status, 303);

    let mut mallory = s.login("mallory", PW);
    let csrf = csrf_token(&mallory.get("/-/settings/profile").body);
    let token = create_token(&mut mallory, "probe", &["repo:write", "user:read"]);
    let mut people = [
        Who {
            name: "anonymous",
            client: s.client(),
            csrf: None,
            token: None,
        },
        Who {
            name: "mallory",
            client: mallory,
            csrf,
            token: Some(token),
        },
    ];
    let before = fingerprint(&s, &git);
    let mut failures = Vec::new();
    for (template, access) in route_table() {
        if template == "/-/design" || template == "/metrics" {
            continue; // debug-only page; separate listener
        }
        let target = concrete(template, false, &head);
        let twin = concrete(template, true, &head);
        for method in ["GET", "POST", "PUT", "PATCH", "DELETE"] {
            for who in &mut people {
                // Signing in or out would change who mallory is.
                if who.name == "mallory" && (template == "/-/logout" || template == "/-/login") {
                    continue;
                }
                let mut r = who.send(method, &target);
                if who.name == "mallory" && r.body.contains("come from this site") {
                    // An earlier request (e.g. "sign out everywhere") ended
                    // her session; sign in again and retry.
                    who.client = s.login("mallory", PW);
                    who.csrf = csrf_token(&who.client.get("/-/settings/profile").body);
                    r = who.send(method, &target);
                }
                let ctx = format!(
                    "{} {method} {target} ({access:?}) -> {}",
                    who.name, r.status
                );
                if r.status >= 500 {
                    failures.push(format!("{ctx}: server error"));
                    continue;
                }
                if who.name == "mallory" && r.body.contains("come from this site") {
                    failures.push(format!("{ctx}: CSRF rejected a valid form (test setup)"));
                }
                let success = (200..300).contains(&r.status);
                let read = method == "GET";
                let anon = who.name == "anonymous";
                match access {
                    Access::Public => {}
                    Access::PublicRead => {
                        if !read && success {
                            failures.push(format!("{ctx}: write succeeded"));
                        }
                    }
                    Access::Account => {
                        if anon && success {
                            failures.push(format!("{ctx}: anonymous access"));
                        }
                    }
                    // 405 is decided by routing alone, the same for everyone.
                    _ if r.status == 405 && !matches!(access, Access::Resource | Access::Git) => {}
                    Access::SiteAdmin => {
                        let ok = r.status == 404 || (!read && r.status == 403);
                        if !ok {
                            failures.push(format!("{ctx}: expected 404"));
                        }
                    }
                    // Organizations are public; their private pages refuse.
                    Access::Resource if !template.contains("{repo}") => {
                        if success {
                            failures
                                .push(format!("{ctx}: access to an organization's private page"));
                        }
                    }
                    Access::Resource | Access::Git => {
                        if success {
                            failures.push(format!("{ctx}: access to a private resource"));
                        }
                        let mut missing = who.send(method, &twin);
                        // Pages echo the requested path (e.g. `?next=`).
                        missing.body = missing.body.replace("nothere", "secret");
                        if normalized(&r) != normalized(&missing) {
                            failures.push(format!(
                                "{ctx}: differs from missing {twin} ({}):\n{}\n---\n{}",
                                missing.status, r.body, missing.body
                            ));
                        }
                    }
                }
            }
        }
    }
    assert!(
        failures.is_empty(),
        "{} problems:\n{}",
        failures.len(),
        failures.join("\n")
    );
    assert_eq!(
        fingerprint(&s, &git),
        before,
        "unauthorized requests changed state"
    );
}
