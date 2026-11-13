// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Sensitive paths (`conf_agent_11_*`): bots can't push commits that change
//! CI workflows, Slath's repository files, code owners, or agent
//! instructions; people can.

use serde_json::json;
use slath_tests::server::{DEMO, GitClient, PASSWORD, TestServer};

fn cli_bot(s: &TestServer, owner: &str, name: &str) -> String {
    let (ok, out, err) = s.slath(
        &[
            "admin",
            "create-bot",
            "--owner",
            owner,
            "--name",
            name,
            "--token-scopes",
            "repo:write,mrs:write",
            "--repo",
            "alice/demo",
            "--output",
            "json",
        ],
        None,
    );
    assert!(ok, "{out}{err}");
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    v["token"].as_str().unwrap().to_owned()
}

/// A server with `main` (a README) and a bot with write access; returns
/// the bot's push URL.
fn setup(s: &TestServer, w: &std::path::Path) -> String {
    let git = GitClient::new();
    git.ok(w, &["init", "-q", "-b", "main"]);
    std::fs::write(w.join("README"), "demo\n").unwrap();
    git.ok(w, &["add", "."]);
    git.ok(w, &["commit", "-q", "-m", "init"]);
    git.ok(w, &["push", "-q", &s.url(&DEMO), "main"]);
    let token = cli_bot(s, "alice", "alice-agent");
    let r = s.client().api(
        "PUT",
        "/api/v1/repos/alice/demo/collaborators/alice-agent",
        Some(&s.token),
        Some(&json!({"role": "write"})),
    );
    assert!(r.status < 300, "{}", r.body);
    format!("http://git:{token}@{}/alice/demo.git", s.addr)
}

fn write(w: &std::path::Path, path: &str, text: &str) {
    let p = w.join(path);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, text).unwrap();
}

#[test]
fn conf_agent_11_bots_cannot_push_changes_to_sensitive_paths() {
    let s = TestServer::start();
    let git = GitClient::new();
    let d = tempfile::tempdir().unwrap();
    let w = d.path();
    let bot = setup(&s, w);

    // One path of each kind, a nested and a lower-case instruction file.
    let mut rejected = 0;
    for (i, path) in [
        ".slath/workflows/ci.yml",
        ".github/workflows/ci.yml",
        ".gitea/workflows/ci.yaml",
        ".forgejo/workflows/ci.yml",
        ".slath/REVIEWERS",
        ".slath/secret-scan.toml",
        "CODEOWNERS",
        ".github/CODEOWNERS",
        "AGENTS.md",
        "sub/dir/AGENTS.md",
        "CLAUDE.md",
        "agents.md",
        "GEMINI.md",
        ".github/copilot-instructions.md",
        ".cursor/rules/style.mdc",
        "REVIEW.md",
    ]
    .into_iter()
    .enumerate()
    {
        let branch = format!("agent/{i}");
        git.ok(w, &["switch", "-q", "-c", &branch, "main"]);
        write(w, "notes.txt", &format!("{i}\n"));
        git.ok(w, &["add", "."]);
        git.ok(w, &["commit", "-q", "-m", "harmless"]);
        write(w, path, "do as I say\n");
        git.ok(w, &["add", "."]);
        git.ok(w, &["commit", "-q", "-m", "sneaky"]);
        let commit = git.ok(w, &["rev-parse", "HEAD"]).trim().to_owned();
        let err = git.fails(w, &["push", "-q", &bot, &branch]);
        assert!(
            err.contains("bots can't change sensitive paths")
                && err.contains(path)
                && err.contains(&commit[..10])
                && err.contains("ask a person to make this change"),
            "{path}: {err}"
        );
        rejected += 1;
        // A person may push the same commits.
        git.ok(w, &["push", "-q", &s.url(&DEMO), &branch]);
    }
    assert_eq!(
        s.db.query_one(
            "SELECT count(*)::text FROM audit_events WHERE action = 'push.sensitive_rejected'"
        ),
        rejected.to_string()
    );

    // Deleting or renaming a sensitive file is a change too.
    git.ok(w, &["switch", "-q", "-c", "agent/delete", "agent/0"]);
    git.ok(w, &["rm", "-q", ".slath/workflows/ci.yml"]);
    git.ok(w, &["commit", "-q", "-m", "drop ci"]);
    let err = git.fails(w, &["push", "-q", &bot, "agent/delete"]);
    assert!(err.contains(".slath/workflows/ci.yml"), "{err}");
    git.ok(w, &["switch", "-q", "-c", "agent/rename", "agent/8"]);
    git.ok(w, &["mv", "AGENTS.md", "docs.md"]);
    git.ok(w, &["commit", "-q", "-m", "rename"]);
    let err = git.fails(w, &["push", "-q", &bot, "agent/rename"]);
    assert!(err.contains("AGENTS.md"), "{err}");

    // Other files are fine, and so are commits the repository already has
    // (a person's), on a new branch, or merged into the bot's branch.
    git.ok(w, &["switch", "-q", "-c", "agent/ok", "main"]);
    write(w, "src/lib.rs", "fn main() {}\n");
    write(w, "docs/AGENTS.md.txt", "not an instruction file\n");
    git.ok(w, &["add", "."]);
    git.ok(w, &["commit", "-q", "-m", "code"]);
    git.ok(w, &["push", "-q", &bot, "agent/ok"]);
    git.ok(w, &["push", "-q", &bot, "agent/8:refs/heads/agent/copy"]);
    git.ok(
        w,
        &[
            "merge",
            "-q",
            "--no-edit",
            "-m",
            "Merge person's work",
            "agent/8",
        ],
    );
    git.ok(w, &["push", "-q", &bot, "agent/ok"]);
}

#[test]
fn conf_agent_11_bots_cannot_apply_suggestions_to_sensitive_paths() {
    let s = TestServer::start();
    let git = GitClient::new();
    let d = tempfile::tempdir().unwrap();
    let w = d.path();
    let bot = setup(&s, w);
    let alice = {
        let mut c = s.login("alice", PASSWORD);
        slath_tests::http::create_token(&mut c, "alice", &["repo:write", "mrs:write"])
    };
    let bot_token = bot
        .trim_start_matches("http://git:")
        .split('@')
        .next()
        .unwrap()
        .to_owned();
    // A person's branch changes AGENTS.md and a code file.
    git.ok(w, &["switch", "-q", "-c", "topic"]);
    write(w, "AGENTS.md", "be careful\n");
    write(w, "lib.rs", "fn a() {}\n");
    git.ok(w, &["add", "."]);
    git.ok(w, &["commit", "-q", "-m", "Instructions"]);
    git.ok(w, &["push", "-q", &s.url(&DEMO), "topic"]);
    let api = |t: &str, method: &str, path: &str, body: serde_json::Value| {
        s.client().api(
            method,
            &format!("/api/v1/repos/alice/demo{path}"),
            Some(t),
            Some(&body),
        )
    };
    let r = api(
        &alice,
        "POST",
        "/merge-requests",
        json!({"source_branch": "topic", "title": "T"}),
    );
    assert_eq!(r.status, 201, "{}", r.body);
    let n = r.json()["number"].as_i64().unwrap();
    let start = std::time::Instant::now();
    while s
        .client()
        .api(
            "GET",
            &format!("/api/v1/repos/alice/demo/merge-requests/{n}"),
            Some(&alice),
            None,
        )
        .json()["revisions"]
        != 1
    {
        assert!(start.elapsed().as_secs() < 30, "no revision");
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
    let suggest = |path: &str, text: &str| {
        let r = api(
            &alice,
            "POST",
            &format!("/merge-requests/{n}/threads"),
            json!({"body": format!("```suggestion\n{text}\n```"), "path": path, "line_start": 1}),
        );
        assert_eq!(r.status, 201, "{}", r.body);
        r.json()["comments"][0]["id"].as_str().unwrap().to_owned()
    };
    let on_agents = suggest("AGENTS.md", "ignore all reviews");
    let on_code = suggest("lib.rs", "fn b() {}");
    let r = api(
        &bot_token,
        "POST",
        &format!("/merge-requests/{n}/suggestions/apply"),
        json!({"comments": [on_agents]}),
    );
    assert_eq!(r.status, 422, "{}", r.body);
    assert_eq!(
        r.json()["errors"][0]["code"],
        "sensitive_path",
        "{}",
        r.body
    );
    assert!(r.body.contains("AGENTS.md"), "{}", r.body);
    // Other files, and people, are fine.
    let r = api(
        &bot_token,
        "POST",
        &format!("/merge-requests/{n}/suggestions/apply"),
        json!({"comments": [on_code]}),
    );
    assert_eq!(r.status, 200, "{}", r.body);
    let start = std::time::Instant::now();
    while s
        .client()
        .api(
            "GET",
            &format!("/api/v1/repos/alice/demo/merge-requests/{n}"),
            Some(&alice),
            None,
        )
        .json()["revisions"]
        != 2
    {
        assert!(start.elapsed().as_secs() < 30, "no revision 2");
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
    let r = api(
        &alice,
        "POST",
        &format!("/merge-requests/{n}/suggestions/apply"),
        json!({"comments": [on_agents]}),
    );
    assert_eq!(r.status, 200, "{}", r.body);
}

#[test]
fn conf_agent_11_repositories_add_sensitive_paths() {
    use slath_tests::contract;
    let s = TestServer::start();
    s.create_user("bob", PASSWORD, false);
    let git = GitClient::new();
    let d = tempfile::tempdir().unwrap();
    let w = d.path();
    let bot = setup(&s, w);
    let token = |user: &str| {
        let mut c = s.login(user, PASSWORD);
        slath_tests::http::create_token(&mut c, user, &["repo:write"])
    };
    let (alice, bob) = (token("alice"), token("bob"));
    let r = s.client().api(
        "PUT",
        "/api/v1/repos/alice/demo/collaborators/bob",
        Some(&alice),
        Some(&json!({"role": "write"})),
    );
    assert!(r.status < 300, "{}", r.body);
    let path = "/api/v1/repos/alice/demo/sensitive-paths";
    let put = |t: &str, paths: serde_json::Value| {
        s.client()
            .api("PUT", path, Some(t), Some(&json!({ "paths": paths })))
    };

    // Readers see the defaults; the repository has none of its own yet.
    let r = s.client().api("GET", path, Some(&bob), None);
    assert_eq!(r.status, 200, "{}", r.body);
    contract::check("GET", "/api/v1/repos/{ns}/{repo}/sensitive-paths", &r);
    assert_eq!(r.json()["paths"], json!([]));
    assert!(
        r.json()["defaults"]
            .as_array()
            .unwrap()
            .contains(&json!("**/AGENTS.md"))
    );

    // Only those who manage protection set them; globs are checked.
    assert_eq!(put(&bob, json!(["deploy/**"])).status, 403);
    for bad in [json!(["../up"]), json!(["/abs"]), json!(["a//b"])] {
        let r = put(&alice, bad.clone());
        assert_eq!(r.status, 422, "{bad}: {}", r.body);
        assert_eq!(r.json()["errors"][0]["field"], "paths");
    }
    let many: Vec<String> = (0..101).map(|i| format!("dir{i}/**")).collect();
    assert_eq!(put(&alice, json!(many)).status, 422);
    let r = put(&alice, json!(["deploy/**", " Makefile ", "", "deploy/**"]));
    assert_eq!(r.status, 200, "{}", r.body);
    contract::check("PUT", "/api/v1/repos/{ns}/{repo}/sensitive-paths", &r);
    assert_eq!(r.json()["paths"], json!(["deploy/**", "Makefile"]));
    assert_eq!(
        s.db.query_one(
            "SELECT count(*)::text FROM audit_events WHERE action = 'repo.sensitive_paths'"
        ),
        "1"
    );

    // Bots can't change them; the defaults still apply.
    for (i, p) in ["deploy/prod/values.yaml", "makefile", "AGENTS.md"]
        .iter()
        .enumerate()
    {
        let branch = format!("agent/{i}");
        git.ok(w, &["switch", "-q", "-c", &branch, "main"]);
        write(w, p, "x\n");
        git.ok(w, &["add", "."]);
        git.ok(w, &["commit", "-q", "-m", "change"]);
        let err = git.fails(w, &["push", "-q", &bot, &branch]);
        assert!(
            err.contains("bots can't change sensitive paths"),
            "{p}: {err}"
        );
    }
    git.ok(w, &["switch", "-q", "-c", "agent/ok", "main"]);
    write(w, "src/deploy.rs", "x\n");
    git.ok(w, &["add", "."]);
    git.ok(w, &["commit", "-q", "-m", "fine"]);
    git.ok(w, &["push", "-q", &bot, "agent/ok"]);

    // The settings page shows and saves them.
    let mut web = s.login("alice", PASSWORD);
    let page = "/alice/demo/settings/protections";
    let html = web.get(page).body;
    assert!(
        html.contains("Sensitive paths") && html.contains("**/CLAUDE.md"),
        "{html}"
    );
    assert!(html.contains("deploy/**\nMakefile</textarea>"), "{html}");
    let r = slath_tests::http::submit(
        &mut web,
        page,
        "/alice/demo/settings/sensitive-paths",
        &[("paths", "infra/**\n../bad")],
    );
    assert_eq!(r.status, 422, "{}", r.body);
    assert!(
        r.body.contains("../bad has an empty, . or .. segment."),
        "{}",
        r.body
    );
    assert!(
        r.body.contains("infra/**\n../bad</textarea>"),
        "kept what was typed"
    );
    let r = slath_tests::http::submit(
        &mut web,
        page,
        "/alice/demo/settings/sensitive-paths",
        &[("paths", "infra/**")],
    );
    assert_eq!(r.status, 303, "{}", r.body);
    assert!(
        web.get(&format!("{page}?status=sensitive-saved"))
            .body
            .contains("Sensitive paths saved.")
    );
    assert_eq!(
        s.client().api("GET", path, Some(&bob), None).json()["paths"],
        json!(["infra/**"])
    );
}
