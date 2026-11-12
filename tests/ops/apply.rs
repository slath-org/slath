// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! `slath admin apply` (OPS-08): a declarative bootstrap that's idempotent
//! and leaves an agent with a working, repository-restricted bot token.

use std::os::unix::fs::PermissionsExt;

use serde_json::{Value, json};
use slath_tests::server::{GitClient, TestServer};

const SEED: &str = r#"
[[users]]
username = "ada"
email = "ada@example.org"
admin = true
password_file = "secrets/ada-password"

[[orgs]]
name = "acme"
owners = ["ada"]

[[bots]]
name = "acme-agent"
owner = "acme"

[[repos]]
namespace = "acme"
name = "app"
collaborators = { "acme-agent" = "write" }

[[tokens]]
name = "agent"
bot = "acme-agent"
scopes = ["repo:write", "mrs:write"]
repos = ["acme/app"]
file = "secrets/bot-token"
"#;

fn apply(s: &TestServer, file: &std::path::Path) -> Value {
    let (ok, out, err) = s.slath(
        &["admin", "apply", file.to_str().unwrap(), "--output", "json"],
        None,
    );
    assert!(ok, "{out}{err}");
    serde_json::from_str(&out).unwrap()
}

#[test]
fn conf_ops_08_apply_is_idempotent_and_yields_a_working_bot_token() {
    let s = TestServer::start_with(&[]);
    let dir = tempfile::tempdir().unwrap();
    let seed = dir.path().join("seed.toml");
    std::fs::write(&seed, SEED).unwrap();

    let first = apply(&s, &seed);
    let kinds: Vec<&str> = first["changes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["kind"].as_str().unwrap())
        .collect();
    assert_eq!(
        kinds,
        ["user", "org", "bot", "repo", "collaborator", "token"],
        "{first}"
    );
    // Secrets land in their files only, readable by the caller alone.
    assert!(!first.to_string().contains("slath_bot_"), "{first}");
    for f in ["secrets/ada-password", "secrets/bot-token"] {
        let meta = std::fs::metadata(dir.path().join(f)).unwrap();
        assert_eq!(meta.permissions().mode() & 0o777, 0o600, "{f}");
    }
    let token = std::fs::read_to_string(dir.path().join("secrets/bot-token")).unwrap();
    let token = token.trim().to_owned();
    assert!(token.starts_with("slath_bot_"), "{token}");

    // A second run changes nothing; a lost token file brings a new token.
    assert_eq!(apply(&s, &seed)["changes"], json!([]));
    std::fs::remove_file(dir.path().join("secrets/bot-token")).unwrap();
    let again = apply(&s, &seed);
    assert_eq!(again["changes"][0]["kind"], "token");
    assert_eq!(again["changes"][0]["action"], "updated");
    let new = std::fs::read_to_string(dir.path().join("secrets/bot-token")).unwrap();
    assert_ne!(new.trim(), token);
    let mut api = s.client();
    assert_eq!(
        api.api("GET", "/api/v1/user", Some(&token), None).status,
        401
    );
    let token = new.trim().to_owned();

    // The admin's password works.
    let pw = std::fs::read_to_string(dir.path().join("secrets/ada-password")).unwrap();
    let _admin = s.login("ada", pw.trim());

    // The bot clones, pushes a branch, and opens a merge request.
    let git = GitClient::new();
    let work = tempfile::tempdir().unwrap();
    let src = work.path().join("app");
    let url = format!("http://agent:{token}@{}/acme/app.git", s.addr);
    git.ok(work.path(), &["clone", "-q", &url, "app"]);
    std::fs::write(src.join("README.md"), "# app\n").unwrap();
    git.ok(&src, &["add", "."]);
    git.ok(&src, &["commit", "-q", "-m", "Start"]);
    git.ok(&src, &["push", "-q", "origin", "HEAD:main"]);
    std::fs::write(src.join("README.md"), "# app\n\nBy an agent.\n").unwrap();
    git.ok(&src, &["commit", "-q", "-am", "Explain"]);
    git.ok(&src, &["push", "-q", "origin", "HEAD:agent/readme"]);
    let r = api.api(
        "POST",
        "/api/v1/repos/acme/app/merge-requests",
        Some(&token),
        Some(&json!({"source_branch": "agent/readme", "title": "Explain the app"})),
    );
    assert_eq!(r.status, 201, "{}", r.body);
}

#[test]
fn conf_ops_08_apply_refuses_bad_plans_without_partial_secrets() {
    let s = TestServer::start_with(&[]);
    let dir = tempfile::tempdir().unwrap();
    let seed = dir.path().join("seed.toml");
    for bad in [
        "[[teams]]\nname = \"x\"\n",
        "[[bots]]\nname = \"b\"\nowner = \"nobody\"\n",
        "[[users]]\nusername = \"u\"\n[[tokens]]\nname = \"t\"\nuser = \"u\"\nbot = \"b\"\nscopes = [\"repo:read\"]\nfile = \"t\"\n",
    ] {
        std::fs::write(&seed, bad).unwrap();
        let (ok, out, err) = s.slath(
            &["admin", "apply", seed.to_str().unwrap(), "--output", "json"],
            None,
        );
        assert!(!ok, "{bad}: {out}");
        let v: Value = serde_json::from_str(&out).unwrap_or_else(|_| json!({"err": err}));
        assert_ne!(v["ok"], true, "{bad}");
    }
    assert!(!dir.path().join("t").exists());
}
