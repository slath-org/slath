// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Starts a `slath serve` with a fresh database and data directory for the
//! browser tests, seeds test accounts and a repository, prints
//! `SLATH_E2E_URL=<base url>`, and runs until stdin closes (the Playwright
//! launcher holds the pipe open).
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::io::{Read, Write};

use std::path::{Path, PathBuf};
use std::process::Command;

use slath_tests::server::{DEMO, GitClient, Instance, PASSWORD, TestServer};

/// The Playwright projects (tests/e2e/playwright.config.ts): each gets its
/// own branch to open a merge request from, so parallel runs don't collide.
const PROJECTS: [&str; 12] = [
    "chromium-light-desktop",
    "chromium-dark-desktop",
    "chromium-light-touch",
    "chromium-dark-touch",
    "firefox-light-desktop",
    "firefox-dark-desktop",
    "firefox-light-touch",
    "firefox-dark-touch",
    "webkit-light-desktop",
    "webkit-dark-desktop",
    "webkit-light-touch",
    "webkit-dark-touch",
];

/// Branch `diffs`: a commit with 26 changed files (more than load with the
/// page), a generated one, and one that only changes indentation.
fn seed_diffs(git: &GitClient, w: &Path, url: &str) {
    git.ok(w, &["switch", "-q", "-c", "diffs", "main"]);
    std::fs::write(w.join("ws.txt"), "alpha\nbeta\n").unwrap();
    git.ok(w, &["add", "-A"]);
    git.ok(w, &["commit", "-q", "-m", "Add ws.txt"]);
    std::fs::write(w.join("ws.txt"), "  alpha\n  beta\n").unwrap();
    std::fs::write(w.join(".gitattributes"), "gen.txt linguist-generated\n").unwrap();
    std::fs::write(w.join("gen.txt"), "generated output\n").unwrap();
    for i in 0..23 {
        std::fs::write(w.join(format!("f{i:02}.txt")), format!("file {i}\n")).unwrap();
    }
    git.ok(w, &["add", "-A"]);
    git.ok(w, &["commit", "-q", "-m", "Change many files"]);
    git.ok(w, &["push", "-q", url, "diffs"]);
    git.ok(w, &["switch", "-q", "main"]);
}

/// Branches `e2e/<project>` to open merge requests from, the merge request
/// "Review fixture" from `review` with two revisions, and `pushed`, which
/// no test opens a merge request from (the "Recently pushed" banner).
fn seed_merge_requests(server: &TestServer, git: &GitClient, w: &Path, url: &str) {
    for p in PROJECTS {
        git.ok(w, &["switch", "-q", "-c", &format!("e2e/{p}"), "main"]);
        std::fs::write(w.join(format!("{p}.md")), format!("# Notes for {p}\n")).unwrap();
        git.ok(w, &["add", "-A"]);
        git.ok(w, &["commit", "-q", "-m", &format!("Add notes for {p}")]);
        git.ok(w, &["push", "-q", url, &format!("e2e/{p}")]);
    }
    git.ok(w, &["switch", "-q", "-c", "review", "main"]);
    std::fs::write(w.join("review.md"), "First draft.\n").unwrap();
    git.ok(w, &["add", "-A"]);
    git.ok(w, &["commit", "-q", "-m", "Add review notes"]);
    git.ok(w, &["push", "-q", url, "review"]);
    let mut alice = server.login("alice", PASSWORD);
    let token = slath_tests::http::create_token(&mut alice, "e2e", &["repo:write", "mrs:write"]);
    let mut api = server.client();
    let r = api.api(
        "POST",
        "/api/v1/repos/alice/demo/merge-requests",
        Some(&token),
        Some(&serde_json::json!({
            "source_branch": "review",
            "title": "Review fixture",
            "description": "Two revisions for the timeline tests.",
        })),
    );
    assert_eq!(r.status, 201, "{}", r.body);
    let n = r.json()["number"].as_i64().unwrap();
    std::fs::write(w.join("review.md"), "Second draft.\n").unwrap();
    git.ok(w, &["commit", "-qam", "Revise review notes"]);
    git.ok(w, &["push", "-q", url, "review"]);
    // The second revision is recorded by a background job.
    let path = format!("/api/v1/repos/alice/demo/merge-requests/{n}");
    let start = std::time::Instant::now();
    while api.api("GET", &path, Some(&token), None).json()["revisions"] != 2 {
        assert!(
            start.elapsed().as_secs() < 60,
            "no second revision\n{}",
            server.log()
        );
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
    // A thread on the second revision's line 1 of review.md.
    let r = api.api(
        "POST",
        &format!("/api/v1/repos/alice/demo/merge-requests/{n}/threads"),
        Some(&token),
        Some(&serde_json::json!({"body": "Is this the final wording?", "path": "review.md", "line_start": 1})),
    );
    assert_eq!(r.status, 201, "{}", r.body);
    // A suggestion on the same line (never applied by the tests).
    let r = api.api(
        "POST",
        &format!("/api/v1/repos/alice/demo/merge-requests/{n}/threads"),
        Some(&token),
        Some(&serde_json::json!({"body": "Shorter:\n\n```suggestion\nSecond.\n```", "path": "review.md", "line_start": 1})),
    );
    assert_eq!(r.status, 201, "{}", r.body);
    // A finding on the same line, reported by the bot e2e-lint.
    let (ok, out, err) = server.slath(
        &[
            "admin",
            "create-bot",
            "--owner",
            "alice",
            "--name",
            "e2e-lint",
            "--token-scopes",
            "repo:read,statuses:write",
            "--repo",
            "alice/demo",
            "--output",
            "json",
        ],
        None,
    );
    assert!(ok, "{out}{err}");
    let bot: serde_json::Value = serde_json::from_str(&out).unwrap();
    let r = api.api(
        "PUT",
        "/api/v1/repos/alice/demo/collaborators/e2e-lint",
        Some(&token),
        Some(&serde_json::json!({"role": "write"})),
    );
    assert!(r.status < 300, "{}", r.body);
    let head = git.ok(w, &["rev-parse", "HEAD"]).trim().to_owned();
    let r = api.api(
        "POST",
        &format!("/api/v1/repos/alice/demo/statuses/{head}"),
        bot["token"].as_str(),
        Some(
            &serde_json::json!({"state": "failure", "context": "lint", "annotations": [
                {"path": "review.md", "line_start": 1, "severity": "warning", "category": "docs",
                 "message": "Say what changed in the draft.\nKeep it to one line."}
            ]}),
        ),
    );
    assert_eq!(r.status, 201, "{}", r.body);
    // "Agent fixture": a bot's merge request with three iterations, left
    // iterating (no test marks it ready).
    let (ok, out, err) = server.slath(
        &[
            "admin",
            "create-bot",
            "--owner",
            "alice",
            "--name",
            "e2e-agent",
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
    let agent: serde_json::Value = serde_json::from_str(&out).unwrap();
    let agent_token = agent["token"].as_str().unwrap().to_owned();
    let r = api.api(
        "PUT",
        "/api/v1/repos/alice/demo/collaborators/e2e-agent",
        Some(&token),
        Some(&serde_json::json!({"role": "write"})),
    );
    assert!(r.status < 300, "{}", r.body);
    let agent_url = format!("http://git:{agent_token}@{}/alice/demo.git", server.addr);
    git.ok(w, &["switch", "-q", "-c", "agent", "main"]);
    let mut agent_mr = None;
    for (k, text) in [
        (1, "First try.\n"),
        (2, "Second try.\n"),
        (3, "Third try.\n"),
    ] {
        std::fs::write(w.join("agent-notes.txt"), text).unwrap();
        git.ok(w, &["add", "-A"]);
        git.ok(w, &["commit", "-q", "-m", &format!("Agent iteration {k}")]);
        git.ok(w, &["push", "-q", &agent_url, "agent"]);
        let n = *agent_mr.get_or_insert_with(|| {
            let r = api.api(
                "POST",
                "/api/v1/repos/alice/demo/merge-requests",
                Some(&agent_token),
                Some(&serde_json::json!({"source_branch": "agent", "title": "Agent fixture"})),
            );
            assert_eq!(r.status, 201, "{}", r.body);
            r.json()["number"].as_i64().unwrap()
        });
        let path = format!("/api/v1/repos/alice/demo/merge-requests/{n}");
        let start = std::time::Instant::now();
        while api.api("GET", &path, Some(&token), None).json()["revisions"] != k {
            assert!(
                start.elapsed().as_secs() < 60,
                "no revision {k}\n{}",
                server.log()
            );
            std::thread::sleep(std::time::Duration::from_millis(200));
        }
    }
    // On it, alice's question, which the bot marked addressed.
    let n = agent_mr.unwrap();
    let r = api.api(
        "POST",
        &format!("/api/v1/repos/alice/demo/merge-requests/{n}/threads"),
        Some(&token),
        Some(&serde_json::json!({"body": "Does this cover the second case?", "path": "agent-notes.txt", "line_start": 1})),
    );
    assert_eq!(r.status, 201, "{}", r.body);
    let thread = r.json()["id"].as_str().unwrap().to_owned();
    let r = api.api(
        "PUT",
        &format!("/api/v1/repos/alice/demo/merge-requests/{n}/threads/{thread}/addressed"),
        Some(&agent_token),
        None,
    );
    assert_eq!(r.status, 204, "{}", r.body);
    // Pushed last, so the "Recently pushed" banner always shows it.
    git.ok(w, &["switch", "-q", "-c", "pushed", "main"]);
    std::fs::write(w.join("pushed.md"), "Pushed last.\n").unwrap();
    git.ok(w, &["add", "-A"]);
    git.ok(w, &["commit", "-q", "-m", "Add a pushed note"]);
    git.ok(w, &["push", "-q", url, "pushed"]);
    git.ok(w, &["switch", "-q", "main"]);
}

/// The jj binary to seed jj-made stacks with: the first of
/// `$SLATH_TEST_JJ`, else the newest `scripts/install-jj.sh` installed.
fn jj_binary() -> PathBuf {
    if let Some(list) = std::env::var_os("SLATH_TEST_JJ")
        && let Some(first) = std::env::split_paths(&list).next()
    {
        return first;
    }
    let dir = std::env::var_os("SLATH_TOOLS_DIR").map_or_else(
        || PathBuf::from(std::env::var_os("HOME").unwrap()).join(".cache/slath/tools"),
        PathBuf::from,
    );
    let mut found: Vec<(Vec<u32>, PathBuf)> = std::fs::read_dir(&dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            let v = name.strip_prefix("jj-")?;
            let key = v.split('.').map(|n| n.parse().unwrap_or(0)).collect();
            let bin = e.path().join("bin/jj");
            bin.is_file().then_some((key, bin))
        })
        .collect();
    found.sort();
    found
        .pop()
        .map(|(_, b)| b)
        .expect("no jj for the browser tests' jj-made stacks: run `just tools`")
}

/// A jj "machine" with its own configuration.
struct Jj {
    bin: PathBuf,
    home: tempfile::TempDir,
}

impl Jj {
    fn new() -> Self {
        let home = tempfile::tempdir().unwrap();
        std::fs::write(
            home.path().join("jj.toml"),
            // The Git client's identity, so the stacks differ only by tool.
            "[user]\nname = \"Test\"\nemail = \"test@example.org\"\n[ui]\npaginate = \"never\"\n",
        )
        .unwrap();
        Self {
            bin: jj_binary(),
            home,
        }
    }

    fn ok(&self, cwd: &Path, args: &[&str]) {
        let out = Command::new(&self.bin)
            .current_dir(cwd)
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", self.home.path())
            .env("JJ_CONFIG", self.home.path().join("jj.toml"))
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "jj {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

/// For each project, a stack of two changes made with Git
/// (`ux/<project>/git`), with jj (`…/jj`), and mixed (`…/mixed`: a Git
/// commit under a jj change), and a stack of five Git changes
/// (`stack/<project>`), with no merge request yet; and
/// `ux/<project>/conflict`, which conflicts with `main`.
fn seed_stacks(git: &GitClient, w: &Path, url: &str) {
    let jj = Jj::new();
    let j = jj.home.path().join("repo");
    jj.ok(jj.home.path(), &["git", "clone", url, j.to_str().unwrap()]);
    let files =
        |p: &str, kind: &str| (format!("ux-{p}-{kind}-a.md"), format!("ux-{p}-{kind}-b.md"));
    for p in PROJECTS {
        // With Git.
        let (a, b) = files(p, "git");
        git.ok(w, &["switch", "-q", "-c", &format!("ux/{p}/git"), "main"]);
        std::fs::write(w.join(&a), "Alpha\n").unwrap();
        git.ok(w, &["add", "-A"]);
        git.ok(w, &["commit", "-q", "-m", "Add alpha"]);
        std::fs::write(w.join(&b), "Beta\n").unwrap();
        git.ok(w, &["add", "-A"]);
        git.ok(w, &["commit", "-q", "-m", "Add beta"]);
        git.ok(w, &["push", "-q", url, &format!("ux/{p}/git")]);
        // With jj.
        let (a, b) = files(p, "jj");
        let branch = format!("ux/{p}/jj");
        jj.ok(&j, &["new", "main@origin", "-m", "Add alpha"]);
        std::fs::write(j.join(&a), "Alpha\n").unwrap();
        jj.ok(&j, &["new", "-m", "Add beta"]);
        std::fs::write(j.join(&b), "Beta\n").unwrap();
        jj.ok(&j, &["bookmark", "create", &branch, "-r", "@"]);
        jj.ok(&j, &["git", "push", "--bookmark", &branch]);
        // Mixed: alpha with Git, beta with jj on top.
        let (a, b) = files(p, "mixed");
        let branch = format!("ux/{p}/mixed");
        git.ok(w, &["switch", "-q", "-c", &branch, "main"]);
        std::fs::write(w.join(&a), "Alpha\n").unwrap();
        git.ok(w, &["add", "-A"]);
        git.ok(w, &["commit", "-q", "-m", "Add alpha"]);
        git.ok(w, &["push", "-q", url, &branch]);
        jj.ok(&j, &["git", "fetch"]);
        jj.ok(&j, &["bookmark", "track", &format!("{branch}@origin")]);
        jj.ok(&j, &["new", &branch, "-m", "Add beta"]);
        std::fs::write(j.join(&b), "Beta\n").unwrap();
        jj.ok(&j, &["bookmark", "set", &branch, "-r", "@"]);
        jj.ok(&j, &["git", "push", "--bookmark", &branch]);
        // A stack of five changes, for moving through a stack.
        git.ok(w, &["switch", "-q", "-c", &format!("stack/{p}"), "main"]);
        for i in 1..=5 {
            std::fs::write(w.join(format!("stack-{p}-{i}.md")), format!("Step {i}\n")).unwrap();
            git.ok(w, &["add", "-A"]);
            git.ok(w, &["commit", "-q", "-m", &format!("Step {i}")]);
        }
        git.ok(w, &["push", "-q", url, &format!("stack/{p}")]);
        // A branch whose file main changes too.
        let shared = format!("ux-{p}-shared.md");
        git.ok(w, &["switch", "-q", "main"]);
        std::fs::write(w.join(&shared), "Base\n").unwrap();
        git.ok(w, &["add", "-A"]);
        git.ok(w, &["commit", "-q", "-m", &format!("Add {shared}")]);
        git.ok(w, &["push", "-q", url, "main"]);
        git.ok(w, &["switch", "-q", "-c", &format!("ux/{p}/conflict")]);
        std::fs::write(w.join(&shared), "From the branch\n").unwrap();
        git.ok(w, &["commit", "-qam", "Change it on the branch"]);
        git.ok(w, &["push", "-q", url, &format!("ux/{p}/conflict")]);
        git.ok(w, &["switch", "-q", "main"]);
        std::fs::write(w.join(&shared), "From main\n").unwrap();
        git.ok(w, &["commit", "-qam", "Change it on main"]);
        git.ok(w, &["push", "-q", url, "main"]);
    }
}

/// "Outcomes fixture": a merge request whose third revision has a change
/// unchanged, one returned, one new, and one removed.
fn seed_outcomes(server: &TestServer, git: &GitClient, w: &Path, url: &str) {
    git.ok(w, &["switch", "-q", "-c", "outcomes", "main"]);
    let mut commits = Vec::new();
    for name in ["alpha", "beta", "gamma"] {
        std::fs::write(w.join(format!("outcomes-{name}.md")), format!("{name}\n")).unwrap();
        git.ok(w, &["add", "-A"]);
        git.ok(w, &["commit", "-q", "-m", &format!("Add {name}")]);
        commits.push(git.ok(w, &["rev-parse", "HEAD"]).trim().to_owned());
    }
    git.ok(w, &["push", "-q", url, "outcomes"]);
    let mut alice = server.login("alice", PASSWORD);
    let token =
        slath_tests::http::create_token(&mut alice, "outcomes", &["repo:write", "mrs:write"]);
    let mut api = server.client();
    let r = api.api(
        "POST",
        "/api/v1/repos/alice/demo/merge-requests",
        Some(&token),
        Some(&serde_json::json!({"source_branch": "outcomes", "title": "Outcomes fixture"})),
    );
    assert_eq!(r.status, 201, "{}", r.body);
    let n = r.json()["number"].as_i64().unwrap();
    let mut wait = |k: i64| {
        let start = std::time::Instant::now();
        while api
            .api(
                "GET",
                &format!("/api/v1/repos/alice/demo/merge-requests/{n}"),
                Some(&token),
                None,
            )
            .json()["revisions"]
            != k
        {
            assert!(start.elapsed().as_secs() < 30, "no revision {k}");
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
    };
    wait(1);
    // Revision 2 drops beta; revision 3 brings it back, drops gamma, and
    // adds delta.
    git.ok(w, &["reset", "-q", "--hard", &commits[0]]);
    git.ok(w, &["cherry-pick", &commits[2]]);
    git.ok(w, &["push", "-q", "--force", url, "outcomes"]);
    wait(2);
    git.ok(w, &["reset", "-q", "--hard", &commits[1]]);
    std::fs::write(w.join("outcomes-delta.md"), "delta\n").unwrap();
    git.ok(w, &["add", "-A"]);
    git.ok(w, &["commit", "-q", "-m", "Add delta"]);
    git.ok(w, &["push", "-q", "--force", url, "outcomes"]);
    wait(3);
    git.ok(w, &["switch", "-q", "main"]);
}

/// "jj conflict fixture": a merge request of three changes made by jj,
/// the second with a conflict that the third inherits. jj refuses to push
/// conflicts, so Git pushes them from jj's store.
fn seed_jj_conflict(server: &TestServer, git: &GitClient, url: &str) {
    let jj = Jj::new();
    let j = jj.home.path().join("conflict");
    jj.ok(jj.home.path(), &["git", "clone", url, j.to_str().unwrap()]);
    jj.ok(&j, &["new", "main@origin", "-m", "Add kappa"]);
    std::fs::write(j.join("conflict-kappa.md"), "kappa\n").unwrap();
    jj.ok(&j, &["new", "-m", "Conflict base"]);
    std::fs::write(j.join("conflict-shared.md"), "one\n").unwrap();
    jj.ok(&j, &["new", "-m", "Change shared"]);
    std::fs::write(j.join("conflict-shared.md"), "two\n").unwrap();
    jj.ok(&j, &["new", "-m", "Add top"]);
    std::fs::write(j.join("conflict-top.md"), "top\n").unwrap();
    // Abandoning the base leaves the edit of a file that isn't there.
    jj.ok(&j, &["abandon", "description(substring:\"Conflict base\")"]);
    jj.ok(&j, &["bookmark", "create", "jj-conflict", "-r", "@"]);
    let store = if j.join(".git").is_dir() {
        j.join(".git")
    } else {
        j.join(".jj/repo/store/git")
    };
    git.ok(&store, &["push", "-q", url, "jj-conflict"]);
    let mut alice = server.login("alice", PASSWORD);
    let token =
        slath_tests::http::create_token(&mut alice, "conflict", &["repo:write", "mrs:write"]);
    let r = server.client().api(
        "POST",
        "/api/v1/repos/alice/demo/merge-requests",
        Some(&token),
        Some(&serde_json::json!({"source_branch": "jj-conflict", "title": "jj conflict fixture"})),
    );
    assert_eq!(r.status, 201, "{}", r.body);
}

/// "Performance fixture": a merge request with 50 changed files and 200
/// comments, for the conversation tab's time budget.
fn seed_performance(server: &TestServer, git: &GitClient, w: &Path, url: &str) {
    git.ok(w, &["switch", "-q", "-c", "perf", "main"]);
    for i in 0..50 {
        std::fs::write(
            w.join(format!("perf-{i:02}.txt")),
            format!("line {i}\n").repeat(20),
        )
        .unwrap();
    }
    git.ok(w, &["add", "-A"]);
    git.ok(w, &["commit", "-q", "-m", "Change fifty files"]);
    git.ok(w, &["push", "-q", url, "perf"]);
    git.ok(w, &["switch", "-q", "main"]);
    let mut alice = server.login("alice", PASSWORD);
    let token = slath_tests::http::create_token(&mut alice, "perf", &["repo:write", "mrs:write"]);
    let mut api = server.client();
    let r = api.api(
        "POST",
        "/api/v1/repos/alice/demo/merge-requests",
        Some(&token),
        Some(&serde_json::json!({"source_branch": "perf", "title": "Performance fixture"})),
    );
    assert_eq!(r.status, 201, "{}", r.body);
    let n = r.json()["number"].as_i64().unwrap();
    for i in 0..200 {
        let r = api.api(
            "POST",
            &format!("/api/v1/repos/alice/demo/merge-requests/{n}/comments"),
            Some(&token),
            Some(&serde_json::json!({"body": format!("Comment {i}: looks fine, with `code` and a [link](#x).")})),
        );
        assert_eq!(r.status, 201, "{}", r.body);
    }
}

fn main() {
    // alice/demo (private) with content; accounts as in tests/e2e/fixtures.ts.
    // A host name, not an IP address: passkeys (WebAuthn) need one.
    // Webhooks may reach a local receiver (nothing listens there).
    let inst = Instance::new("[webhooks]\nallow_http = true\nallow_networks = [\"127.0.0.0/8\"]\n");
    let origin = inst.use_localhost();
    let mut server = inst.start(&[]);
    server.seed_repos(&[DEMO]);
    server.create_user("sysadmin", "e2e admin password", true);
    let git = GitClient::new();
    let work = tempfile::tempdir().unwrap();
    let w = work.path();
    git.ok(w, &["init", "-q", "-b", "main"]);
    std::fs::create_dir_all(w.join("src")).unwrap();
    std::fs::write(
        w.join("README.md"),
        "# Demo\n\nA repository for browser tests.\n\n<div hx-get=\"/-/logout\" hx-trigger=\"load\">injected</div>\n\n- [x] item\n\n```rust\nfn main() {}\n```\n",
    )
    .unwrap();
    std::fs::write(
        w.join("src/main.rs"),
        "fn main() {\n    println!(\"hello\");\n}\n",
    )
    .unwrap();
    git.ok(w, &["add", "-A"]);
    git.ok(w, &["commit", "-q", "-m", "Initial commit"]);
    std::fs::write(
        w.join("src/main.rs"),
        "fn main() {\n    println!(\"hello, world\");\n}\n",
    )
    .unwrap();
    git.ok(w, &["commit", "-qam", "Greet the world"]);
    let url = server.url(&DEMO);
    git.ok(w, &["push", "-q", &url, "main"]);
    // The stacks first: the "Recently pushed" banner shows the last push.
    seed_stacks(&git, w, &url);
    seed_performance(&server, &git, w, &url);
    seed_merge_requests(&server, &git, w, &url);
    seed_diffs(&git, w, &url);
    // After the others, so their merge request numbers stay.
    seed_outcomes(&server, &git, w, &url);
    seed_jj_conflict(&server, &git, &url);
    // The browser tests sign in as alice with this password.
    assert_eq!(PASSWORD, "slath test password");
    println!("SLATH_E2E_URL={origin}");
    std::io::stdout().flush().unwrap();
    let mut sink = Vec::new();
    let _ = std::io::stdin().read_to_end(&mut sink);
    drop(server);
}
