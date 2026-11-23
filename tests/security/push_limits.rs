// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! The per-bot push limit (`[limits] bot_pushes_per_hour`): a bot's pushes
//! over HTTP and SSH count against one limit, the push over it is refused
//! with a message that says when to try again, and people's pushes are
//! never limited.

use std::path::Path;

use serde_json::json;
use slath_tests::http::create_token;
use slath_tests::server::{GitClient, TestServer};
use slath_tests::sshd::{self, Sshd};

const PW: &str = "correct horse battery staple";

/// alice/app, alice's bot alice-ci with write access and a token; returns
/// (alice's token, the bot's token).
fn setup(s: &TestServer) -> (String, String) {
    s.create_user("alice", PW, false);
    let mut a = s.login("alice", PW);
    slath_tests::http::create_repo(&mut a, "alice", "app", "private");
    let ta = create_token(&mut a, "t", &["user:write", "repo:write"]);
    let (ok, out, err) = s.slath(
        &[
            "admin",
            "create-bot",
            "--owner",
            "alice",
            "--name",
            "alice-ci",
            "--token-scopes",
            "repo:write,user:read",
            "--repo",
            "alice/app",
            "--output",
            "json",
        ],
        None,
    );
    assert!(ok, "{out}{err}");
    let bot = serde_json::from_str::<serde_json::Value>(&out).unwrap()["token"]
        .as_str()
        .unwrap()
        .to_owned();
    let r = s.client().api(
        "PUT",
        "/api/v1/repos/alice/app/collaborators/alice-ci",
        Some(&ta),
        Some(&json!({"role": "write"})),
    );
    assert!(r.status < 300, "{}", r.body);
    (ta, bot)
}

fn http_url(s: &TestServer, token: &str) -> String {
    format!(
        "http://git:{token}@{}/alice/app.git",
        s.base().trim_start_matches("http://")
    )
}

/// Pushes a new branch to `url`; (accepted, stderr).
fn push(url: &str, branch: &str, ssh: Option<(&Sshd, &Path)>) -> (bool, String) {
    let git = GitClient::new();
    let d = tempfile::tempdir().unwrap();
    let w = d.path();
    git.ok(w, &["init", "-q", "-b", branch]);
    std::fs::write(w.join("f.txt"), branch).unwrap();
    git.ok(w, &["add", "."]);
    git.ok(w, &["commit", "-q", "-m", branch]);
    let mut cmd = git.cmd(w);
    cmd.args(["push", "-q", url, branch]);
    if let Some((sshd, key)) = ssh {
        cmd.env("GIT_SSH_COMMAND", sshd.git_ssh_command(key));
    }
    let out = cmd.output().unwrap();
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn conf_perf_04_a_bot_over_its_push_limit_is_refused_people_are_not() {
    let s = TestServer::start_full(&[], "[limits]\nbot_pushes_per_hour = 2\n");
    let (ta, bot) = setup(&s);
    let bot_url = http_url(&s, &bot);
    assert!(push(&bot_url, "one", None).0);
    assert!(push(&bot_url, "two", None).0);
    let (ok, err) = push(&bot_url, "three", None);
    assert!(!ok);
    assert!(
        err.contains("this bot has used its 2 pushes per hour; try again in"),
        "{err}"
    );
    // The refused push changed nothing.
    let r = s
        .client()
        .api("GET", "/api/v1/repos/alice/app/branches", Some(&ta), None);
    let names: Vec<_> = r.json()["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|b| b["name"].as_str().unwrap().to_owned())
        .collect();
    assert!(!names.contains(&"three".to_owned()), "{names:?}");
    // Its owner pushes as often as they like.
    let alice_url = http_url(&s, &ta);
    for b in ["a1", "a2", "a3", "a4"] {
        let (ok, err) = push(&alice_url, b, None);
        assert!(ok, "{err}");
    }
}

#[test]
fn conf_perf_04_pushes_over_http_and_ssh_share_one_limit() {
    if sshd::skipped() {
        eprintln!("SLATH_SKIP_SSH_TESTS=1: skipping");
        return;
    }
    let s = TestServer::start_full(&[], "[limits]\nbot_pushes_per_hour = 2\n");
    let (ta, bot) = setup(&s);
    let keys = tempfile::tempdir().unwrap();
    let (key, public) = sshd::keygen(keys.path(), "ci");
    let r = s.client().api(
        "POST",
        "/api/v1/bots/alice-ci/keys",
        Some(&ta),
        Some(&json!({"title": "ci", "key": public})),
    );
    assert_eq!(r.status, 201, "{}", r.body);
    let sshd = Sshd::start(&s);
    let ssh_url = sshd.url("alice", "app");
    let via_ssh = Some((&sshd, key.as_path()));
    assert!(push(&ssh_url, "one", via_ssh).0);
    assert!(push(&http_url(&s, &bot), "two", None).0);
    let (ok, err) = push(&ssh_url, "three", via_ssh);
    assert!(!ok);
    assert!(err.contains("pushes per hour; try again in"), "{err}");
}
