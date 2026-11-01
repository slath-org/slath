// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! The REST API: every endpoint with positive and negative cases, checked
//! against the committed OpenAPI document; pagination; idempotency.

use serde_json::json;
use sha2::Digest;
use slath_tests::contract::check;
use slath_tests::http::{Client, create_token};
use slath_tests::server::{DEMO, GitClient, PASSWORD, TestServer};

struct Env {
    s: TestServer,
    c: Client,
    /// alice: repo:write, user:write, user:read.
    alice: String,
    /// bob: same scopes, no access to alice's repositories.
    bob: String,
}

fn setup() -> Env {
    let s = TestServer::start();
    let git = GitClient::new();
    let d = tempfile::tempdir().unwrap();
    let w = d.path().join("w");
    std::fs::create_dir_all(w.join("src")).unwrap();
    git.ok(&w, &["init", "-q", "-b", "main"]);
    std::fs::write(w.join("README.md"), "# Demo\n").unwrap();
    std::fs::write(w.join("src/lib.rs"), "pub fn f() {}\n").unwrap();
    git.ok(&w, &["add", "-A"]);
    git.ok(&w, &["commit", "-q", "-m", "first"]);
    git.ok(&w, &["tag", "v1"]);
    git.ok(&w, &["push", "-q", &s.url(&DEMO), "main", "v1"]);
    s.create_user("bob", PASSWORD, false);
    let mut a = s.login("alice", PASSWORD);
    let alice = create_token(&mut a, "api", &["repo:write", "user:write"]);
    let mut b = s.login("bob", PASSWORD);
    let bob = create_token(&mut b, "api", &["repo:write", "user:write"]);
    let c = s.client();
    Env { s, c, alice, bob }
}

impl Env {
    fn call(
        &mut self,
        method: &str,
        template: &str,
        path: &str,
        token: Option<&str>,
        body: Option<serde_json::Value>,
    ) -> slath_tests::http::Resp {
        let r = self.c.api(method, path, token, body.as_ref());
        check(method, template, &r);
        r
    }
}

#[test]
fn conf_api_01_every_endpoint_matches_the_contract() {
    let mut e = setup();
    let (a, b) = (e.alice.clone(), e.bob.clone());
    let key = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../crates/auth/tests/data/ssh/ed25519.pub"),
    )
    .unwrap();
    // Meta and user.
    assert_eq!(
        e.call("GET", "/api/v1/version", "/api/v1/version", None, None)
            .status,
        200
    );
    assert_eq!(
        e.call("GET", "/api/v1/user", "/api/v1/user", Some(&a), None)
            .status,
        200
    );
    assert_eq!(
        e.call("GET", "/api/v1/user", "/api/v1/user", None, None)
            .status,
        401
    );
    // Keys.
    let k = e.call(
        "POST",
        "/api/v1/user/keys",
        "/api/v1/user/keys",
        Some(&a),
        Some(json!({"key": key.trim()})),
    );
    assert_eq!(k.status, 201, "{}", k.body);
    let key_id = k.json()["id"].as_str().unwrap().to_owned();
    assert_eq!(
        e.call(
            "POST",
            "/api/v1/user/keys",
            "/api/v1/user/keys",
            Some(&a),
            Some(json!({"key": "ssh-dss AAAA"}))
        )
        .status,
        422
    );
    assert_eq!(
        e.call(
            "GET",
            "/api/v1/user/keys",
            "/api/v1/user/keys",
            Some(&a),
            None
        )
        .json()["items"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        e.call(
            "DELETE",
            "/api/v1/user/keys/{id}",
            &format!("/api/v1/user/keys/{key_id}"),
            Some(&b),
            None
        )
        .status,
        404
    );
    assert_eq!(
        e.call(
            "DELETE",
            "/api/v1/user/keys/{id}",
            &format!("/api/v1/user/keys/{key_id}"),
            Some(&a),
            None
        )
        .status,
        204
    );
    // Tokens: listed, never minted through the API.
    let t = e.call(
        "GET",
        "/api/v1/user/tokens",
        "/api/v1/user/tokens",
        Some(&a),
        None,
    );
    assert_eq!(t.status, 200);
    assert!(!t.body.contains("slath_pat_"), "secrets never appear");
    assert_eq!(
        e.call(
            "POST",
            "/api/v1/user/tokens",
            "/api/v1/user/tokens",
            Some(&a),
            None
        )
        .status,
        403
    );
    let tid = t.json()["items"][0]["id"].as_str().unwrap().to_owned();
    assert_eq!(
        e.call(
            "DELETE",
            "/api/v1/user/tokens/{id}",
            &format!("/api/v1/user/tokens/{tid}"),
            Some(&b),
            None
        )
        .status,
        404
    );
    // Organizations.
    assert_eq!(
        e.call(
            "POST",
            "/api/v1/orgs",
            "/api/v1/orgs",
            Some(&a),
            Some(json!({"name": "acme"}))
        )
        .status,
        201
    );
    assert_eq!(
        e.call(
            "POST",
            "/api/v1/orgs",
            "/api/v1/orgs",
            Some(&b),
            Some(json!({"name": "ACME"}))
        )
        .status,
        422
    );
    assert_eq!(
        e.call("GET", "/api/v1/orgs", "/api/v1/orgs", Some(&a), None)
            .json()["items"][0]["name"],
        "acme"
    );
    assert_eq!(
        e.call("GET", "/api/v1/orgs/{org}", "/api/v1/orgs/acme", None, None)
            .status,
        200
    );
    assert_eq!(
        e.call("GET", "/api/v1/orgs/{org}", "/api/v1/orgs/nope", None, None)
            .status,
        404
    );
    assert_eq!(
        e.call(
            "PATCH",
            "/api/v1/orgs/{org}",
            "/api/v1/orgs/acme",
            Some(&b),
            Some(json!({"description": "x"}))
        )
        .status,
        403
    );
    assert_eq!(
        e.call(
            "PATCH",
            "/api/v1/orgs/{org}",
            "/api/v1/orgs/acme",
            Some(&a),
            Some(json!({"description": "Tools"}))
        )
        .json()["description"],
        "Tools"
    );
    assert_eq!(
        e.call(
            "GET",
            "/api/v1/orgs/{org}/members",
            "/api/v1/orgs/acme/members",
            Some(&b),
            None
        )
        .status,
        404
    );
    assert_eq!(
        e.call(
            "PUT",
            "/api/v1/orgs/{org}/members/{user}",
            "/api/v1/orgs/acme/members/bob",
            Some(&a),
            Some(json!({"role": "member"}))
        )
        .status,
        200
    );
    assert_eq!(
        e.call(
            "GET",
            "/api/v1/orgs/{org}/members",
            "/api/v1/orgs/acme/members",
            Some(&b),
            None
        )
        .json()["items"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert_eq!(
        e.call(
            "GET",
            "/api/v1/orgs/{org}/members/{user}",
            "/api/v1/orgs/acme/members/bob",
            Some(&b),
            None
        )
        .json()["role"],
        "member"
    );
    assert_eq!(
        e.call(
            "PUT",
            "/api/v1/orgs/{org}/members/{user}",
            "/api/v1/orgs/acme/members/alice",
            Some(&a),
            Some(json!({"role": "member"}))
        )
        .status,
        409
    );
    assert_eq!(
        e.call(
            "DELETE",
            "/api/v1/orgs/{org}/members/{user}",
            "/api/v1/orgs/acme/members/bob",
            Some(&a),
            None
        )
        .status,
        204
    );
    // Repositories.
    let r = e.call(
        "POST",
        "/api/v1/repos",
        "/api/v1/repos",
        Some(&a),
        Some(json!({"namespace": "alice", "name": "api-made", "visibility": "public"})),
    );
    assert_eq!(r.status, 201, "{}", r.body);
    assert_eq!(r.header("location"), Some("/api/v1/repos/alice/api-made"));
    assert_eq!(
        e.call(
            "POST",
            "/api/v1/repos",
            "/api/v1/repos",
            Some(&b),
            Some(json!({"namespace": "alice", "name": "x", "visibility": "public"}))
        )
        .status,
        422
    );
    assert_eq!(
        e.call(
            "POST",
            "/api/v1/repos",
            "/api/v1/repos",
            None,
            Some(json!({"namespace": "alice", "name": "x", "visibility": "public"}))
        )
        .status,
        401
    );
    let mine = e
        .call("GET", "/api/v1/repos", "/api/v1/repos", Some(&a), None)
        .json();
    assert_eq!(mine["items"].as_array().unwrap().len(), 2);
    let anon = e
        .call("GET", "/api/v1/repos", "/api/v1/repos", None, None)
        .json();
    assert_eq!(
        anon["items"].as_array().unwrap().len(),
        1,
        "only the public one"
    );
    for (who, status) in [
        (Some(a.as_str()), 200),
        (Some(b.as_str()), 404),
        (None, 404),
    ] {
        assert_eq!(
            e.call(
                "GET",
                "/api/v1/repos/{ns}/{repo}",
                "/api/v1/repos/alice/demo",
                who,
                None
            )
            .status,
            status
        );
        assert_eq!(
            e.call(
                "GET",
                "/api/v1/repos/{ns}/{repo}/branches",
                "/api/v1/repos/alice/demo/branches",
                who,
                None
            )
            .status,
            status
        );
        assert_eq!(
            e.call(
                "GET",
                "/api/v1/repos/{ns}/{repo}/tags",
                "/api/v1/repos/alice/demo/tags",
                who,
                None
            )
            .status,
            status
        );
        assert_eq!(
            e.call(
                "GET",
                "/api/v1/repos/{ns}/{repo}/commits",
                "/api/v1/repos/alice/demo/commits?ref=main&path=src",
                who,
                None
            )
            .status,
            status
        );
        assert_eq!(
            e.call(
                "GET",
                "/api/v1/repos/{ns}/{repo}/tree/{ref}",
                "/api/v1/repos/alice/demo/tree/main?path=src",
                who,
                None
            )
            .status,
            status
        );
        assert_eq!(
            e.call(
                "GET",
                "/api/v1/repos/{ns}/{repo}/blob/{ref}",
                "/api/v1/repos/alice/demo/blob/v1?path=src/lib.rs",
                who,
                None
            )
            .status,
            status
        );
    }
    let commits = e
        .call(
            "GET",
            "/api/v1/repos/{ns}/{repo}/commits",
            "/api/v1/repos/alice/demo/commits",
            Some(&a),
            None,
        )
        .json();
    let oid = commits["items"][0]["oid"].as_str().unwrap().to_owned();
    let detail = e.call(
        "GET",
        "/api/v1/repos/{ns}/{repo}/commits/{oid}",
        &format!("/api/v1/repos/alice/demo/commits/{oid}"),
        Some(&a),
        None,
    );
    assert_eq!(detail.json()["files"].as_array().unwrap().len(), 2);
    assert_eq!(
        e.call(
            "GET",
            "/api/v1/repos/{ns}/{repo}/commits/{oid}",
            &format!("/api/v1/repos/alice/demo/commits/{oid}"),
            Some(&b),
            None
        )
        .status,
        404
    );
    let blob = e
        .call(
            "GET",
            "/api/v1/repos/{ns}/{repo}/blob/{ref}",
            "/api/v1/repos/alice/demo/blob/main?path=README.md",
            Some(&a),
            None,
        )
        .json();
    assert_eq!(blob["content"], "# Demo\n");
    assert_eq!(
        e.call(
            "GET",
            "/api/v1/repos/{ns}/{repo}/blob/{ref}",
            "/api/v1/repos/alice/demo/blob/main?path=../etc/passwd",
            Some(&a),
            None
        )
        .status,
        404
    );
    // Collaborators and settings.
    assert_eq!(
        e.call(
            "GET",
            "/api/v1/repos/{ns}/{repo}/collaborators",
            "/api/v1/repos/alice/demo/collaborators",
            Some(&b),
            None
        )
        .status,
        404
    );
    assert_eq!(
        e.call(
            "PUT",
            "/api/v1/repos/{ns}/{repo}/collaborators/{user}",
            "/api/v1/repos/alice/demo/collaborators/bob",
            Some(&a),
            Some(json!({"role": "read"}))
        )
        .status,
        200
    );
    assert_eq!(
        e.call(
            "GET",
            "/api/v1/repos/{ns}/{repo}",
            "/api/v1/repos/alice/demo",
            Some(&b),
            None
        )
        .status,
        200
    );
    assert_eq!(
        e.call(
            "PATCH",
            "/api/v1/repos/{ns}/{repo}",
            "/api/v1/repos/alice/demo",
            Some(&b),
            Some(json!({"description": "x"}))
        )
        .status,
        403
    );
    assert_eq!(
        e.call(
            "GET",
            "/api/v1/repos/{ns}/{repo}/collaborators",
            "/api/v1/repos/alice/demo/collaborators",
            Some(&a),
            None
        )
        .json()["items"][0]["role"],
        "read"
    );
    assert_eq!(
        e.call(
            "DELETE",
            "/api/v1/repos/{ns}/{repo}/collaborators/{user}",
            "/api/v1/repos/alice/demo/collaborators/bob",
            Some(&a),
            None
        )
        .status,
        204
    );
    let p = e.call(
        "PATCH",
        "/api/v1/repos/{ns}/{repo}",
        "/api/v1/repos/alice/api-made",
        Some(&a),
        Some(json!({"description": "Made by API", "visibility": "private", "archived": true})),
    );
    assert_eq!(p.status, 200, "{}", p.body);
    assert_eq!(p.json()["archived"], true);
    assert_eq!(
        e.call(
            "DELETE",
            "/api/v1/repos/{ns}/{repo}",
            "/api/v1/repos/alice/api-made",
            Some(&b),
            None
        )
        .status,
        404
    );
    // Archived repositories refuse everything but unarchiving, deletion included.
    assert_eq!(
        e.call(
            "DELETE",
            "/api/v1/repos/{ns}/{repo}",
            "/api/v1/repos/alice/api-made",
            Some(&a),
            None
        )
        .status,
        403
    );
    assert_eq!(
        e.call(
            "PATCH",
            "/api/v1/repos/{ns}/{repo}",
            "/api/v1/repos/alice/api-made",
            Some(&a),
            Some(json!({"archived": false}))
        )
        .status,
        200
    );
    assert_eq!(
        e.call(
            "DELETE",
            "/api/v1/repos/{ns}/{repo}",
            "/api/v1/repos/alice/api-made",
            Some(&a),
            None
        )
        .status,
        204
    );
    assert_eq!(
        e.call(
            "GET",
            "/api/v1/repos/{ns}/{repo}",
            "/api/v1/repos/alice/api-made",
            Some(&a),
            None
        )
        .status,
        404
    );
}

#[test]
fn conf_api_01_renames_redirect_readers_only() {
    let mut e = setup();
    let a = e.alice.clone();
    let b = e.bob.clone();
    let r = e.call(
        "PATCH",
        "/api/v1/repos/{ns}/{repo}",
        "/api/v1/repos/alice/demo",
        Some(&a),
        Some(json!({"name": "renamed"})),
    );
    assert_eq!(r.json()["name"], "renamed");
    let moved = e.c.api(
        "GET",
        "/api/v1/repos/alice/demo/branches?limit=1",
        Some(&a),
        None,
    );
    assert_eq!(moved.status, 301);
    assert_eq!(
        moved.header("location"),
        Some("/api/v1/repos/alice/renamed/branches?limit=1")
    );
    let hidden = e.c.api("GET", "/api/v1/repos/alice/demo", Some(&b), None);
    assert_eq!(hidden.status, 404);
    assert_eq!(hidden.header("location"), None);
}

#[test]
fn conf_api_01_pagination_uses_signed_cursors() {
    let mut e = setup();
    let a = e.alice.clone();
    for i in 0..5 {
        e.c.api(
            "POST",
            "/api/v1/repos",
            Some(&a),
            Some(&json!({"namespace": "alice", "name": format!("r{i}"), "visibility": "private"})),
        );
    }
    let mut seen = Vec::new();
    let mut url = "/api/v1/repos?limit=2".to_owned();
    loop {
        let r = e.call("GET", "/api/v1/repos", &url, Some(&a), None);
        let j = r.json();
        seen.extend(
            j["items"]
                .as_array()
                .unwrap()
                .iter()
                .map(|i| i["full_name"].as_str().unwrap().to_owned()),
        );
        match j["next_cursor"].as_str() {
            Some(c) => {
                assert!(r.header("link").unwrap().contains("rel=\"next\""));
                url = format!("/api/v1/repos?limit=2&cursor={c}");
            }
            None => break,
        }
    }
    assert_eq!(seen.len(), 6, "{seen:?}");
    // Tampered cursors are rejected.
    let r = e.c.api(
        "GET",
        "/api/v1/repos?cursor=eyJvIjoxfQ.forged",
        Some(&a),
        None,
    );
    assert_eq!(r.status, 422);
    assert_eq!(r.json()["code"], "validation_failed");
}

#[test]
fn conf_api_05_idempotency_keys() {
    let mut e = setup();
    let a = e.alice.clone();
    let body = json!({"namespace": "alice", "name": "once", "visibility": "private"});
    let first = e.c.api_with(
        "POST",
        "/api/v1/repos",
        Some(&a),
        Some(&body),
        &[("idempotency-key", "create-once-1")],
    );
    assert_eq!(first.status, 201);
    assert_eq!(first.header("idempotency-replayed"), None);
    // An identical retry replays the stored response without a second repository.
    let again = e.c.api_with(
        "POST",
        "/api/v1/repos",
        Some(&a),
        Some(&body),
        &[("idempotency-key", "create-once-1")],
    );
    assert_eq!(again.status, 201);
    assert_eq!(again.header("idempotency-replayed"), Some("true"));
    assert_eq!(again.json()["id"], first.json()["id"]);
    assert_eq!(
        e.s.db
            .query_one("SELECT count(*)::text FROM repositories WHERE name = 'once'"),
        "1"
    );
    // The same key with a different body is refused.
    let other = json!({"namespace": "alice", "name": "twice", "visibility": "private"});
    let r = e.c.api_with(
        "POST",
        "/api/v1/repos",
        Some(&a),
        Some(&other),
        &[("idempotency-key", "create-once-1")],
    );
    assert_eq!(r.status, 422);
    assert_eq!(r.json()["code"], "idempotency_key_reused");
    // A duplicate while the first request still runs gets 409.
    let body =
        serde_json::to_vec(&json!({"namespace": "alice", "name": "slow", "visibility": "private"}))
            .unwrap();
    let mut h = sha2::Sha256::new();
    h.update(b"POST\0/api/v1/repos\0");
    h.update(&body);
    let digest: String = h.finalize().iter().map(|b| format!("{b:02x}")).collect();
    e.s.db.execute(&format!(
        "INSERT INTO idempotency_keys (principal_id, key, method, path, request_sha256)
         SELECT user_id, 'in-flight', 'POST', '/api/v1/repos', decode('{digest}', 'hex') FROM access_tokens LIMIT 1"
    ));
    let alice_id = e.s.db.query_one("SELECT u.id::text FROM users u JOIN namespaces n ON n.id = u.namespace_id WHERE n.name = 'alice'");
    e.s.db.execute(&format!(
        "UPDATE idempotency_keys SET principal_id = '{alice_id}' WHERE key = 'in-flight'"
    ));
    let r = e.c.request(
        "POST",
        "/api/v1/repos",
        &[
            ("authorization", &format!("Bearer {a}")),
            ("idempotency-key", "in-flight"),
            ("cookie", ""),
        ],
        Some(("application/json", body)),
    );
    assert_eq!(r.status, 409, "{}", r.body);
    assert_eq!(r.json()["code"], "idempotency_in_progress");
    // Malformed keys are rejected.
    let r = e.c.api_with(
        "POST",
        "/api/v1/repos",
        Some(&a),
        Some(&body_of("x")),
        &[("idempotency-key", "has space")],
    );
    assert_eq!(r.status, 422);
}

fn body_of(name: &str) -> serde_json::Value {
    json!({"namespace": "alice", "name": name, "visibility": "private"})
}

#[test]
fn conf_api_01_rate_limits_are_reported_and_enforced() {
    let s = TestServer::start_full(&[], "[limits]\napi_anonymous_requests_per_hour = 3\n");
    let mut c = s.client();
    let r = c.api("GET", "/api/v1/version", None, None);
    assert_eq!(r.header("ratelimit-limit"), Some("3"));
    assert_eq!(r.header("ratelimit-remaining"), Some("2"));
    c.api("GET", "/api/v1/version", None, None);
    c.api("GET", "/api/v1/version", None, None);
    let r = c.api("GET", "/api/v1/version", None, None);
    assert_eq!(r.status, 429);
    assert!(r.header("retry-after").is_some());
    assert_eq!(r.json()["code"], "rate_limited");
}
