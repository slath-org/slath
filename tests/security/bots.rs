// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Bot accounts (`conf_auth_06_*`): created by their owner, never signed in,
//! capped by their owner, and constrained by guardrails.

use serde_json::json;
use slath_tests::contract;
use slath_tests::http::{Client, create_token, login, submit};
use slath_tests::server::TestServer;

const PW: &str = "correct horse battery staple";

fn create_org(c: &mut Client, name: &str) {
    let r = submit(
        c,
        "/-/orgs/new",
        "/-/orgs",
        &[("name", name), ("display_name", "")],
    );
    assert_eq!(r.status, 303, "{}", r.body);
}

#[test]
fn conf_auth_06_bots_are_created_and_seen_by_their_owner_only() {
    let s = TestServer::start_with(&[]);
    s.create_user("alice", PW, false);
    s.create_user("bob", PW, false);
    let mut a = s.login("alice", PW);
    let mut b = s.login("bob", PW);
    let ta = create_token(&mut a, "t", &["user:write"]);
    let tb = create_token(&mut b, "t", &["user:write"]);
    let mut api = s.client();

    let r = api.api(
        "POST",
        "/api/v1/user/bots",
        Some(&ta),
        Some(&json!({"username": "alice-ci", "display_name": "Alice's CI"})),
    );
    assert_eq!(r.status, 201, "{}", r.body);
    contract::check("POST", "/api/v1/user/bots", &r);
    let bot = r.json();
    assert_eq!(bot["owner"], json!({"kind": "user", "name": "alice"}));
    assert_eq!(bot["state"], "active");

    let r = api.api("GET", "/api/v1/user/bots", Some(&ta), None);
    assert_eq!(r.status, 200);
    contract::check("GET", "/api/v1/user/bots", &r);
    assert_eq!(r.json()["items"][0]["username"], "alice-ci");
    let r = api.api("GET", "/api/v1/bots/alice-ci", Some(&ta), None);
    assert_eq!(r.status, 200);
    contract::check("GET", "/api/v1/bots/{name}", &r);

    // Someone else's bot doesn't exist for bob, and bob's list is empty.
    assert_eq!(
        api.api("GET", "/api/v1/bots/alice-ci", Some(&tb), None)
            .status,
        404
    );
    let r = api.api("GET", "/api/v1/user/bots", Some(&tb), None);
    assert_eq!(r.json()["items"], json!([]));
    // A user isn't a bot.
    assert_eq!(
        api.api("GET", "/api/v1/bots/alice", Some(&ta), None).status,
        404
    );

    // Bot names share the namespace with users and organizations.
    for taken in ["bob", "ALICE-CI", "api"] {
        let r = api.api(
            "POST",
            "/api/v1/user/bots",
            Some(&ta),
            Some(&json!({"username": taken})),
        );
        assert_eq!(r.status, 422, "{taken}: {}", r.body);
    }
    // Anonymous requests can't create bots.
    let r = api.api(
        "POST",
        "/api/v1/user/bots",
        None,
        Some(&json!({"username": "x"})),
    );
    assert_eq!(r.status, 401);
}

#[test]
fn conf_auth_06_organization_bots_need_an_organization_owner() {
    let s = TestServer::start_with(&[]);
    s.create_user("alice", PW, false);
    s.create_user("bob", PW, false);
    let mut a = s.login("alice", PW);
    create_org(&mut a, "acme");
    let r = submit(
        &mut a,
        "/acme/-/members",
        "/acme/-/members",
        &[("username", "bob"), ("role", "member")],
    );
    assert!(r.status == 303 || r.status == 200, "{}", r.body);
    let ta = create_token(&mut a, "t", &["user:write"]);
    let mut b = s.login("bob", PW);
    let tb = create_token(&mut b, "t", &["user:write"]);
    let mut api = s.client();

    // A member who isn't an owner can't create or list the org's bots.
    let new = json!({"username": "acme-bot"});
    for (m, body) in [("POST", Some(&new)), ("GET", None)] {
        let r = api.api(m, "/api/v1/orgs/acme/bots", Some(&tb), body);
        assert_eq!(r.status, 404, "{m}: {}", r.body);
    }
    let r = api.api("POST", "/api/v1/orgs/acme/bots", Some(&ta), Some(&new));
    assert_eq!(r.status, 201, "{}", r.body);
    contract::check("POST", "/api/v1/orgs/{org}/bots", &r);
    assert_eq!(r.json()["owner"], json!({"kind": "org", "name": "acme"}));
    let r = api.api("GET", "/api/v1/orgs/acme/bots", Some(&ta), None);
    contract::check("GET", "/api/v1/orgs/{org}/bots", &r);
    assert_eq!(r.json()["items"][0]["username"], "acme-bot");
    assert_eq!(
        api.api("GET", "/api/v1/bots/acme-bot", Some(&tb), None)
            .status,
        404
    );
    // Bots can't own organizations.
    let r = submit(
        &mut a,
        "/acme/-/members",
        "/acme/-/members",
        &[("username", "acme-bot"), ("role", "owner")],
    );
    assert_eq!(r.status, 422, "{}", r.body);
    assert!(
        r.body.contains("Bots can&#39;t own organizations."),
        "{}",
        r.body
    );
}

#[test]
fn conf_auth_06_bots_cannot_sign_in() {
    let s = TestServer::start_with(&[]);
    s.create_user("alice", PW, false);
    let mut a = s.login("alice", PW);
    let t = create_token(&mut a, "t", &["user:write"]);
    let r = s.client().api(
        "POST",
        "/api/v1/user/bots",
        Some(&t),
        Some(&json!({"username": "alice-ci"})),
    );
    assert_eq!(r.status, 201);
    // Bots have no password (the database refuses one), and sign-in
    // with a bot's name fails exactly like with an unknown name.
    let mut c = s.client();
    let bot = login(&mut c, "alice-ci", PW);
    let mut c2 = s.client();
    let unknown = login(&mut c2, "nobody", PW);
    assert_eq!(bot.status, unknown.status);
    assert!(
        bot.set_cookies()
            .iter()
            .all(|c| !c.contains("slath_session=") || c.contains("slath_session=;"))
    );
    assert_ne!(c.get("/-/settings/profile").status, 200);
}

/// Creates a bot with `slath admin create-bot` and returns its token.
fn cli_bot(s: &TestServer, owner: &str, name: &str, extra: &[&str]) -> Result<String, String> {
    let mut args = vec![
        "admin",
        "create-bot",
        "--owner",
        owner,
        "--name",
        name,
        "--output",
        "json",
    ];
    args.extend_from_slice(extra);
    let (ok, out, err) = s.slath(&args, None);
    if !ok {
        return Err(format!("{out}{err}"));
    }
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    Ok(v["token"].as_str().unwrap_or_default().to_owned())
}

#[test]
fn conf_auth_06_bot_tokens_are_repo_restricted_and_expire() {
    let s = TestServer::start_with(&[]);
    s.create_user("alice", PW, false);
    let mut a = s.login("alice", PW);
    slath_tests::http::create_repo(&mut a, "alice", "app", "private");
    slath_tests::http::create_repo(&mut a, "alice", "other", "private");

    // Bot tokens must name repositories, may not manage accounts, and
    // can't outlive the configured maximum.
    for bad in [
        &["--token-scopes", "repo:read"][..],
        &["--token-scopes", "user:write", "--repo", "alice/app"],
        &["--token-scopes", "admin", "--repo", "alice/app"],
        &["--token-scopes", "repo:read", "--repo", "alice/nope"],
        &[
            "--token-scopes",
            "repo:read",
            "--repo",
            "alice/app",
            "--token-days",
            "91",
        ],
    ] {
        assert!(cli_bot(&s, "alice", "bad-bot", bad).is_err(), "{bad:?}");
    }
    let token = cli_bot(
        &s,
        "alice",
        "alice-ci",
        &[
            "--token-scopes",
            "repo:write,user:read",
            "--repo",
            "alice/app",
        ],
    )
    .unwrap();
    assert!(token.starts_with("slath_bot_"), "{token}");
    let days = s.db.query_one(
        "SELECT round(extract(epoch FROM expires_at - now()) / 86400)::text FROM access_tokens",
    );
    assert_eq!(days, "30");

    let mut api = s.client();
    // The bot gets a role like anyone else: here on both repositories.
    let tr = create_token(&mut a, "admin", &["repo:write"]);
    for repo in ["app", "other"] {
        let r = api.api(
            "PUT",
            &format!("/api/v1/repos/alice/{repo}/collaborators/alice-ci"),
            Some(&tr),
            Some(&json!({"role": "write"})),
        );
        assert!(r.status < 300, "{}", r.body);
    }
    let r = api.api("GET", "/api/v1/user", Some(&token), None);
    assert_eq!(r.status, 200);
    assert_eq!(r.json()["kind"], "bot");
    assert_eq!(
        api.api("GET", "/api/v1/repos/alice/app", Some(&token), None)
            .status,
        200
    );
    assert_eq!(
        api.api("GET", "/api/v1/repos/alice/other", Some(&token), None)
            .status,
        404
    );
    // Bots can't create bots or organizations.
    for (path, body) in [
        ("/api/v1/user/bots", json!({"username": "sub-bot"})),
        ("/api/v1/orgs", json!({"name": "bot-org"})),
    ] {
        let r = api.api("POST", path, Some(&token), Some(&body));
        assert_eq!(r.status, 403, "{path}: {}", r.body);
    }

    // The owner sees the token, can't create one through the API, and
    // revokes it.
    let tw = create_token(&mut a, "t", &["user:write"]);
    let r = api.api("GET", "/api/v1/bots/alice-ci/tokens", Some(&tw), None);
    assert_eq!(r.status, 200);
    contract::check("GET", "/api/v1/bots/{name}/tokens", &r);
    let id = r.json()["items"][0]["id"].as_str().unwrap().to_owned();
    let r = api.api(
        "POST",
        "/api/v1/bots/alice-ci/tokens",
        Some(&tw),
        Some(&json!({"name": "x", "scopes": ["repo:read"]})),
    );
    assert_eq!(r.status, 403);
    let r = api.api(
        "DELETE",
        &format!("/api/v1/bots/alice-ci/tokens/{id}"),
        Some(&tw),
        None,
    );
    assert_eq!(r.status, 204);
    assert_eq!(
        api.api("GET", "/api/v1/user", Some(&token), None).status,
        401
    );
}

#[test]
fn conf_auth_06_token_kinds_are_bound_to_account_kinds() {
    let s = TestServer::start_with(&[]);
    s.create_user("alice", PW, false);
    let mut a = s.login("alice", PW);
    slath_tests::http::create_repo(&mut a, "alice", "app", "private");
    let pat = create_token(&mut a, "t", &["user:read"]);
    let bot = cli_bot(
        &s,
        "alice",
        "alice-ci",
        &["--token-scopes", "user:read", "--repo", "alice/app"],
    )
    .unwrap();
    let mut api = s.client();
    assert_eq!(api.api("GET", "/api/v1/user", Some(&pat), None).status, 200);
    assert_eq!(api.api("GET", "/api/v1/user", Some(&bot), None).status, 200);
    // Swap the owners of the two tokens: a person's token never
    // authenticates a bot, nor a bot's token a person.
    s.db.execute(
        "UPDATE access_tokens SET user_id = CASE WHEN user_id = (SELECT id FROM users WHERE kind = 'bot') \
           THEN (SELECT id FROM users WHERE kind = 'human') ELSE (SELECT id FROM users WHERE kind = 'bot') END",
    );
    assert_eq!(api.api("GET", "/api/v1/user", Some(&pat), None).status, 401);
    assert_eq!(api.api("GET", "/api/v1/user", Some(&bot), None).status, 401);
}

#[test]
fn conf_auth_06_bot_keys_are_managed_by_the_owner() {
    let s = TestServer::start_with(&[]);
    s.create_user("alice", PW, false);
    s.create_user("bob", PW, false);
    let mut a = s.login("alice", PW);
    let ta = create_token(&mut a, "t", &["user:write"]);
    let mut b = s.login("bob", PW);
    let tb = create_token(&mut b, "t", &["user:write"]);
    cli_bot(&s, "alice", "alice-ci", &[]).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let (_, public) = slath_tests::sshd::keygen(dir.path(), "ci");
    let mut api = s.client();
    let key = json!({"title": "ci", "key": public});
    let r = api.api("POST", "/api/v1/bots/alice-ci/keys", Some(&tb), Some(&key));
    assert_eq!(r.status, 404);
    let r = api.api("POST", "/api/v1/bots/alice-ci/keys", Some(&ta), Some(&key));
    assert_eq!(r.status, 201, "{}", r.body);
    contract::check("POST", "/api/v1/bots/{name}/keys", &r);
    let id = r.json()["id"].as_str().unwrap().to_owned();
    let r = api.api("GET", "/api/v1/bots/alice-ci/keys", Some(&ta), None);
    contract::check("GET", "/api/v1/bots/{name}/keys", &r);
    assert_eq!(r.json()["items"][0]["title"], "ci");
    // The key belongs to the bot, not to its owner.
    let r = api.api("GET", "/api/v1/user/keys", Some(&ta), None);
    assert_eq!(r.json()["items"], json!([]));
    let path = format!("/api/v1/bots/alice-ci/keys/{id}");
    assert_eq!(api.api("DELETE", &path, Some(&tb), None).status, 404);
    assert_eq!(api.api("DELETE", &path, Some(&ta), None).status, 204);
}

#[test]
fn conf_auth_06_a_bot_can_do_no_more_than_its_owner() {
    let s = TestServer::start_with(&[]);
    s.create_user("alice", PW, false);
    s.create_user("bob", PW, false);
    let mut b = s.login("bob", PW);
    slath_tests::http::create_repo(&mut b, "bob", "lib", "private");
    slath_tests::http::create_repo(&mut b, "bob", "secret", "private");
    let tb = create_token(&mut b, "t", &["repo:write"]);
    let bot = cli_bot(
        &s,
        "alice",
        "alice-ci",
        &[
            "--token-scopes",
            "repo:read,issues:write",
            "--repo",
            "bob/lib",
            "--repo",
            "bob/secret",
        ],
    )
    .unwrap();
    let mut api = s.client();
    let role = |api: &mut Client, repo: &str, user: &str, role: &str| {
        let r = api.api(
            "PUT",
            &format!("/api/v1/repos/bob/{repo}/collaborators/{user}"),
            Some(&tb),
            Some(&json!({"role": role})),
        );
        assert!(r.status < 300, "{}", r.body);
    };
    role(&mut api, "lib", "alice-ci", "maintain");
    role(&mut api, "lib", "alice", "read");
    role(&mut api, "secret", "alice-ci", "read");
    let label = |api: &mut Client, name: &str| {
        api.api(
            "POST",
            "/api/v1/repos/bob/lib/labels",
            Some(&bot),
            Some(&json!({"name": name, "color": "#112233"})),
        )
        .status
    };
    // Maintain for the bot, but its owner only reads: the bot only reads.
    assert_eq!(
        api.api("GET", "/api/v1/repos/bob/lib", Some(&bot), None)
            .status,
        200
    );
    assert_eq!(label(&mut api, "capped"), 403);
    // Its owner can't see bob/secret at all, so neither can the bot, in
    // listings included.
    assert_eq!(
        api.api("GET", "/api/v1/repos/bob/secret", Some(&bot), None)
            .status,
        404
    );
    let list = api.api("GET", "/api/v1/repos", Some(&bot), None);
    assert!(!list.body.contains("\"secret\""), "{}", list.body);
    assert!(list.body.contains("\"lib\""), "{}", list.body);
    // Once the owner maintains too, so does the bot.
    role(&mut api, "lib", "alice", "maintain");
    assert_eq!(label(&mut api, "allowed"), 201);
    role(&mut api, "secret", "alice", "read");
    assert_eq!(
        api.api("GET", "/api/v1/repos/bob/secret", Some(&bot), None)
            .status,
        200
    );
}

#[test]
fn conf_auth_06_suspending_the_owner_suspends_their_bots() {
    let s = TestServer::start_with(&[]);
    s.create_user("alice", PW, false);
    s.create_user("boss", PW, true);
    let mut a = s.login("alice", PW);
    slath_tests::http::create_repo(&mut a, "alice", "app", "private");
    let args = ["--token-scopes", "user:read", "--repo", "alice/app"];
    let ci = cli_bot(&s, "alice", "alice-ci", &args).unwrap();
    let other = cli_bot(&s, "alice", "alice-other", &args).unwrap();
    let mut api = s.client();
    let works =
        |api: &mut Client, t: &str| api.api("GET", "/api/v1/user", Some(t), None).status == 200;
    let mut r = s.login("boss", PW);
    let post = |r: &mut Client, path: &str| submit(r, "/-/admin/users", path, &[]).status;

    // A bot suspended on its own stays suspended through its owner's
    // suspension and reactivation.
    assert_eq!(post(&mut r, "/-/admin/users/alice-other/suspend"), 303);
    assert!(!works(&mut api, &other));

    assert_eq!(post(&mut r, "/-/admin/users/alice/suspend"), 303);
    assert!(!works(&mut api, &ci));
    let audit = s.db.query_one(
        "SELECT data->>'bots' FROM audit_events WHERE action = 'admin.user.suspend' ORDER BY created_at DESC LIMIT 1",
    );
    assert_eq!(audit, "1");
    // Nobody reactivates a bot while its owner is suspended.
    assert_ne!(post(&mut r, "/-/admin/users/alice-ci/unsuspend"), 303);
    assert!(!works(&mut api, &ci));

    assert_eq!(post(&mut r, "/-/admin/users/alice/unsuspend"), 303);
    assert!(works(&mut api, &ci), "the bot's token works again");
    assert!(!works(&mut api, &other));
}

#[test]
fn conf_auth_06_bots_merge_and_push_to_protected_branches_only_if_listed() {
    use slath_tests::server::{DEMO, GitClient};
    let s = TestServer::start();
    let git = GitClient::new();
    let work = tempfile::tempdir().unwrap();
    let src = work.path().join("src");
    std::fs::create_dir(&src).unwrap();
    let commit = |file: &str, text: &str| {
        std::fs::write(src.join(file), text).unwrap();
        git.ok(&src, &["add", "."]);
        git.ok(&src, &["commit", "-q", "-m", text]);
    };
    git.ok(&src, &["init", "-q", "-b", "main"]);
    commit("a.txt", "one");
    git.ok(&src, &["push", "-q", &s.url(&DEMO), "main"]);

    let bot = cli_bot(
        &s,
        "alice",
        "alice-ci",
        &[
            "--token-scopes",
            "repo:write,mrs:write",
            "--repo",
            "alice/demo",
        ],
    )
    .unwrap();
    let mut api = s.client();
    let r = api.api(
        "PUT",
        "/api/v1/repos/alice/demo/collaborators/alice-ci",
        Some(&s.token),
        Some(&json!({"role": "write"})),
    );
    assert!(r.status < 300, "{}", r.body);
    let protect = |api: &mut Client, bots: serde_json::Value| {
        api.api(
            "POST",
            "/api/v1/repos/alice/demo/protections",
            Some(&s.token),
            Some(&json!({"pattern": "main", "required_approvals": 0, "bot_merge_allowlist": bots})),
        )
    };
    // Only bots can be listed.
    assert_eq!(protect(&mut api, json!(["alice"])).status, 422);
    let r = protect(&mut api, json!([]));
    assert_eq!(r.status, 200, "{}", r.body);
    contract::check("POST", "/api/v1/repos/{ns}/{repo}/protections", &r);

    let bot_url = format!("http://git:{bot}@{}/alice/demo.git", s.addr);
    // The bot pushes to an unprotected branch, but not to main.
    commit("b.txt", "two");
    git.ok(&src, &["push", "-q", &bot_url, "main:topic"]);
    let err = git.fails(&src, &["push", "-q", &bot_url, "main"]);
    assert!(
        err.contains(
            "bots can merge into or push to this protected branch only if its protection lists them"
        ),
        "{err}"
    );
    // Nor does it merge into main, by hand or when ready.
    let r = api.api(
        "POST",
        "/api/v1/repos/alice/demo/merge-requests",
        Some(&bot),
        Some(&json!({"source_branch": "topic", "title": "Two"})),
    );
    assert_eq!(r.status, 201, "{}", r.body);
    let mr = r.json();
    let (n, head) = (
        mr["number"].as_i64().unwrap(),
        mr["head_oid"].as_str().unwrap().to_owned(),
    );
    let merge = |api: &mut Client| {
        api.api(
            "POST",
            &format!("/api/v1/repos/alice/demo/merge-requests/{n}/merge"),
            Some(&bot),
            Some(&json!({"expected_head_oid": head, "strategy": "fast-forward"})),
        )
    };
    let r = merge(&mut api);
    assert_eq!(r.status, 409, "{}", r.body);
    assert!(
        r.body.contains("only if its protection lists them"),
        "{}",
        r.body
    );
    let r = api.api(
        "PUT",
        &format!("/api/v1/repos/alice/demo/merge-requests/{n}/auto-merge"),
        Some(&bot),
        Some(&json!({})),
    );
    assert_eq!(r.status, 409, "{}", r.body);

    // Listed, it may do both.
    let r = protect(&mut api, json!(["alice-ci"]));
    assert_eq!(r.json()["bot_merge_allowlist"], json!(["alice-ci"]));
    let r = merge(&mut api);
    assert!(r.status < 300, "{}", r.body);
    commit("c.txt", "three");
    git.ok(&src, &["push", "-q", &bot_url, "main"]);
}

#[test]
fn conf_auth_06_bot_approvals_count_only_if_enabled_and_never_for_their_owner() {
    use slath_tests::server::{DEMO, GitClient};
    let s = TestServer::start();
    s.create_user("carol", PW, false);
    let git = GitClient::new();
    let work = tempfile::tempdir().unwrap();
    let src = work.path().join("src");
    std::fs::create_dir(&src).unwrap();
    git.ok(&src, &["init", "-q", "-b", "main"]);
    std::fs::write(src.join("a.txt"), "one").unwrap();
    git.ok(&src, &["add", "."]);
    git.ok(&src, &["commit", "-q", "-m", "one"]);
    git.ok(&src, &["push", "-q", &s.url(&DEMO), "main"]);
    std::fs::write(src.join("a.txt"), "two").unwrap();
    git.ok(&src, &["commit", "-q", "-am", "two"]);
    git.ok(&src, &["push", "-q", &s.url(&DEMO), "main:topic"]);

    let mut api = s.client();
    let args = [
        "--token-scopes",
        "repo:read,mrs:write",
        "--repo",
        "alice/demo",
    ];
    let own = cli_bot(&s, "alice", "alice-ci", &args).unwrap();
    let other = cli_bot(&s, "carol", "carol-ci", &args).unwrap();
    for who in ["carol", "alice-ci", "carol-ci"] {
        let r = api.api(
            "PUT",
            &format!("/api/v1/repos/alice/demo/collaborators/{who}"),
            Some(&s.token),
            Some(&json!({"role": "write"})),
        );
        assert!(r.status < 300, "{}", r.body);
    }
    let protect = |api: &mut Client, bots: bool| {
        let r = api.api(
            "POST",
            "/api/v1/repos/alice/demo/protections",
            Some(&s.token),
            Some(&json!({"pattern": "main", "required_approvals": 1, "bots_count_as_approvers": bots})),
        );
        assert_eq!(r.status, 200, "{}", r.body);
    };
    protect(&mut api, false);
    let mut a = s.login("alice", slath_tests::server::PASSWORD);
    let ta = create_token(&mut a, "mr", &["repo:write", "mrs:write"]);
    let r = api.api(
        "POST",
        "/api/v1/repos/alice/demo/merge-requests",
        Some(&ta),
        Some(&json!({"source_branch": "topic", "title": "Two"})),
    );
    assert_eq!(r.status, 201, "{}", r.body);
    let n = r.json()["number"].as_i64().unwrap();
    for t in [&own, &other] {
        let r = api.api(
            "POST",
            &format!("/api/v1/repos/alice/demo/merge-requests/{n}/reviews"),
            Some(t),
            Some(&json!({"verdict": "approve"})),
        );
        assert_eq!(r.status, 201, "{}", r.body);
    }
    let approvals = |api: &mut Client| {
        let m = api
            .api(
                "GET",
                &format!("/api/v1/repos/alice/demo/merge-requests/{n}/mergeability"),
                Some(&s.token),
                None,
            )
            .json();
        let r = m["requirements"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["key"] == "approvals")
            .unwrap()
            .clone();
        (
            r["state"].as_str().unwrap().to_owned(),
            r["reason"].as_str().unwrap().to_owned(),
        )
    };
    // Bot approvals don't count by default.
    assert_eq!(approvals(&mut api).0, "unmet");
    // Enabled, carol's bot counts, but never the author's own bot.
    protect(&mut api, true);
    let (state, reason) = approvals(&mut api);
    assert_eq!(state, "met", "{reason}");
    assert!(
        reason.contains("carol-ci") && !reason.contains("alice-ci"),
        "{reason}"
    );
}
