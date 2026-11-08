// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Push protection (SEC-08): secrets are rejected before they enter a
//! repository, and nothing of a rejected push becomes visible.

use std::path::Path;

use slath_core::secret_scan::samples;
use slath_tests::server::{DEMO, GitClient, TestServer};
use slath_tests::sshd::{self, Sshd};

fn work(git: &GitClient, dir: &Path) {
    std::fs::create_dir_all(dir).unwrap();
    git.ok(dir, &["init", "-q", "-b", "main"]);
    std::fs::write(dir.join("README.md"), "# demo\n").unwrap();
    git.ok(dir, &["add", "."]);
    git.ok(dir, &["commit", "-q", "-m", "first"]);
}

fn commit_file(git: &GitClient, dir: &Path, name: &str, text: &str) -> String {
    let path = dir.join(name);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, text).unwrap();
    git.ok(dir, &["add", "."]);
    git.ok(dir, &["commit", "-q", "-m", &format!("add {name}")]);
    git.ok(dir, &["rev-parse", "HEAD"]).trim().to_owned()
}

/// Nothing of commit `oid` reached the server: no ref, no object, no push
/// event, no job.
fn invisible(s: &TestServer, git: &GitClient, oid: &str, pushes_before: &str) {
    let bare = s.repo_path(&DEMO);
    let (ok, _) = {
        let out = git
            .cmd(&bare)
            .args(["cat-file", "-e", oid])
            .output()
            .unwrap();
        (out.status.success(), ())
    };
    assert!(!ok, "the rejected commit is in the repository");
    assert!(!git.ok(&bare, &["for-each-ref"]).contains(oid));
    assert_eq!(
        s.db.query_one("SELECT count(*)::text FROM push_events"),
        pushes_before
    );
    assert_eq!(
        s.db.query_one("SELECT count(*)::text FROM jobs WHERE kind = 'ref.updated'"),
        pushes_before
    );
}

#[test]
fn conf_sec_08_every_block_rule_rejects_over_https() {
    let s = TestServer::start();
    let git = GitClient::new();
    let d = tempfile::tempdir().unwrap();
    let w = d.path().join("w");
    work(&git, &w);
    let url = s.url(&DEMO);
    git.ok(&w, &["push", "-q", &url, "main"]);
    let base = git.ok(&w, &["rev-parse", "HEAD"]).trim().to_owned();
    let pushes = s.db.query_one("SELECT count(*)::text FROM push_events");
    for (rule, text) in samples()
        .into_iter()
        .filter(|(r, _)| *r != "generic-assignment")
    {
        let oid = commit_file(&git, &w, &format!("conf/{rule}.txt"), &text);
        let err = git.fails(&w, &["push", &url, "main"]);
        assert!(err.contains("push rejected"), "{rule}: {err}");
        assert!(
            err.contains(&format!("conf/{rule}.txt line ")),
            "{rule}: {err}"
        );
        assert!(err.contains(&format!("({rule})")), "{rule}: {err}");
        assert!(
            err.contains("git rebase -i") && err.contains("jj edit"),
            "{rule}: {err}"
        );
        invisible(&s, &git, &oid, &pushes);
        git.ok(&w, &["reset", "-q", "--hard", &base]);
    }
}

#[test]
fn conf_sec_08_secrets_in_commit_messages_and_over_ssh_are_rejected() {
    if sshd::skipped() {
        return;
    }
    let keys = tempfile::tempdir().unwrap();
    let (key, public) = sshd::keygen(keys.path(), "alice");
    let s = TestServer::start();
    s.add_ssh_key("alice", &public);
    let ssh = Sshd::start(&s);
    let git = GitClient::new();
    let d = tempfile::tempdir().unwrap();
    let w = d.path().join("w");
    work(&git, &w);
    let ssh_push = |args: &[&str]| {
        git.cmd(&w)
            .env("GIT_SSH_COMMAND", ssh.git_ssh_command(&key))
            .args(args)
            .output()
            .unwrap()
    };
    let url = ssh.url("alice", "demo");
    let out = ssh_push(&["push", "-q", &url, "main"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let pushes = s.db.query_one("SELECT count(*)::text FROM push_events");

    let (_, text) = samples()
        .into_iter()
        .find(|(r, _)| *r == "github-token")
        .unwrap();
    let oid = commit_file(&git, &w, "deploy.env", &text);
    let out = ssh_push(&["push", &url, "main"]);
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        !out.status.success() && err.contains("(github-token)"),
        "{err}"
    );
    invisible(&s, &git, &oid, &pushes);

    // In a commit message.
    git.ok(&w, &["reset", "-q", "--hard", "HEAD~1"]);
    git.ok(
        &w,
        &[
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            &format!("deploy\n\n{text}"),
        ],
    );
    let oid = git.ok(&w, &["rev-parse", "HEAD"]).trim().to_owned();
    let out = ssh_push(&["push", &url, "main"]);
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        !out.status.success() && err.contains("message line 3"),
        "{err}"
    );
    invisible(&s, &git, &oid, &pushes);
}

#[test]
fn conf_sec_08_warn_rules_and_warn_mode_dont_block() {
    let s = TestServer::start();
    let git = GitClient::new();
    let d = tempfile::tempdir().unwrap();
    let w = d.path().join("w");
    work(&git, &w);
    let url = s.url(&DEMO);
    let (_, generic) = samples()
        .into_iter()
        .find(|(r, _)| *r == "generic-assignment")
        .unwrap();
    commit_file(&git, &w, "settings.py", &generic);
    let out = git.cmd(&w).args(["push", &url, "main"]).output().unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{err}");
    assert!(
        err.contains("warning") && err.contains("(generic-assignment)"),
        "{err}"
    );

    // A repository in warn mode accepts a block-rule finding with a warning.
    s.db.execute("UPDATE repositories SET push_protection = 'warn'");
    let (_, token) = samples()
        .into_iter()
        .find(|(r, _)| *r == "gitlab-token")
        .unwrap();
    commit_file(&git, &w, "ci.env", &token);
    let out = git.cmd(&w).args(["push", &url, "main"]).output().unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{err}");
    assert!(
        err.contains("(gitlab-token)") && !err.contains("rejected"),
        "{err}"
    );

    // Content already in the repository isn't scanned again.
    s.db.execute("UPDATE repositories SET push_protection = 'block'");
    commit_file(&git, &w, "notes.md", "unrelated\n");
    git.ok(&w, &["push", "-q", &url, "main"]);
}

#[test]
fn conf_sec_08_the_site_minimum_overrides_the_repository() {
    let s = TestServer::start_full(&[DEMO], "[push_protection]\nminimum = \"block\"\n");
    s.db.execute("UPDATE repositories SET push_protection = 'off'");
    let git = GitClient::new();
    let d = tempfile::tempdir().unwrap();
    let w = d.path().join("w");
    work(&git, &w);
    let (_, token) = samples()
        .into_iter()
        .find(|(r, _)| *r == "slath-token")
        .unwrap();
    commit_file(&git, &w, "token.txt", &token);
    let err = git.fails(&w, &["push", &s.url(&DEMO), "main"]);
    assert!(err.contains("(slath-token)"), "{err}");
}

#[test]
fn conf_sec_08_over_limit_scans_reject() {
    let s = TestServer::start_full(
        &[DEMO],
        "[limits]\nsecret_scan_file_max_bytes = \"1 KiB\"\n",
    );
    let git = GitClient::new();
    let d = tempfile::tempdir().unwrap();
    let w = d.path().join("w");
    work(&git, &w);
    let url = s.url(&DEMO);
    git.ok(&w, &["push", "-q", &url, "main"]);
    // A large binary file isn't scanned and passes.
    let mut binary = vec![0u8; 4096];
    binary[100] = 7;
    std::fs::write(w.join("image.bin"), &binary).unwrap();
    git.ok(&w, &["add", "."]);
    git.ok(&w, &["commit", "-q", "-m", "binary"]);
    git.ok(&w, &["push", "-q", &url, "main"]);
    // A large text file can't be scanned and rejects the push.
    commit_file(&git, &w, "big.txt", &"line of text\n".repeat(200));
    let err = git.fails(&w, &["push", &url, "main"]);
    assert!(err.contains("limits.secret_scan_file_max_bytes"), "{err}");
}

#[test]
fn conf_sec_08_repository_admins_choose_the_mode() {
    use slath_tests::http::submit;
    let s = TestServer::start();
    let mut a = s.login("alice", slath_tests::server::PASSWORD);
    let page = a.get("/alice/demo/settings/general");
    assert!(page.body.contains("Push protection"), "{}", page.body);
    assert!(page.body.contains("value=\"block\" checked"));
    let r = submit(
        &mut a,
        "/alice/demo/settings/general",
        "/alice/demo/settings/push-protection",
        &[("mode", "warn")],
    );
    assert_eq!(r.status, 303, "{}", r.body);
    assert_eq!(
        s.db.query_one("SELECT push_protection FROM repositories"),
        "warn"
    );
    assert_eq!(
        s.db.query_one(
            "SELECT count(*)::text FROM audit_events WHERE action = 'repo.push_protection'"
        ),
        "1"
    );
}

/// The allowance link from a rejection, as a path and query.
fn allow_link(err: &str) -> String {
    let line = err
        .lines()
        .find(|l| l.contains("/-/secret-scan/allow?fingerprint="))
        .unwrap_or_else(|| panic!("no allowance link in {err}"));
    let url = line.split_whitespace().last().unwrap();
    url[url.find("/alice/").unwrap()..].to_owned()
}

fn query_value<'a>(link: &'a str, key: &str) -> &'a str {
    link.split(['?', '&'])
        .find_map(|p| p.strip_prefix(&format!("{key}=")))
        .unwrap()
}

#[test]
fn conf_sec_08_an_allowance_lets_exactly_its_finding_through_until_it_expires() {
    use slath_tests::http::submit;
    use slath_tests::server::PASSWORD;
    let s = TestServer::start();
    s.create_user("bob", PASSWORD, false);
    let git = GitClient::new();
    let d = tempfile::tempdir().unwrap();
    let w = d.path().join("w");
    work(&git, &w);
    let url = s.url(&DEMO);
    git.ok(&w, &["push", "-q", &url, "main"]);
    let (_, fixture) = samples()
        .into_iter()
        .find(|(r, _)| *r == "private-key")
        .unwrap();
    commit_file(&git, &w, "tests/data/key.pem", &fixture);
    let err = git.fails(&w, &["push", &url, "main"]);
    let link = allow_link(&err);
    let (fp, rule) = (
        query_value(&link, "fingerprint").to_owned(),
        query_value(&link, "rule").to_owned(),
    );
    assert_eq!(rule, "private-key");

    // Someone who can't push sees nothing.
    let mut bob = s.login("bob", PASSWORD);
    assert_eq!(bob.get(&link).status, 404);
    assert_eq!(
        submit(
            &mut bob,
            "/-/settings/profile",
            "/alice/demo/-/secret-scan/allow",
            &[("fingerprint", &fp), ("rule", &rule), ("days", "7")]
        )
        .status,
        404
    );

    // The writer allows it; the same push now passes.
    let mut alice = s.login("alice", PASSWORD);
    let page = alice.get(&link);
    assert_eq!(page.status, 200);
    assert!(page.body.contains(&fp), "{}", page.body);
    let r = submit(
        &mut alice,
        &link,
        "/alice/demo/-/secret-scan/allow",
        &[
            ("fingerprint", &fp),
            ("rule", &rule),
            ("days", "7"),
            ("reason", "test fixture"),
        ],
    );
    assert_eq!(r.status, 303, "{}", r.body);
    let out = git.cmd(&w).args(["push", &url, "main"]).output().unwrap();
    let text = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{text}");
    assert!(text.contains("allowed (allowance)"), "{text}");
    assert_eq!(
        s.db.query_one(
            "SELECT count(*)::text FROM audit_events WHERE action = 'secret_scan.allow'"
        ),
        "1"
    );

    // A different secret isn't covered.
    let (_, other) = samples()
        .into_iter()
        .find(|(r, _)| *r == "npm-token")
        .unwrap();
    commit_file(&git, &w, "npmrc", &other);
    assert!(
        git.fails(&w, &["push", &url, "main"])
            .contains("(npm-token)")
    );
    git.ok(&w, &["reset", "-q", "--hard", "HEAD~1"]);

    // Expired, it allows nothing.
    s.db.execute("UPDATE secret_allowances SET created_at = now() - interval '9 days', expires_at = now() - interval '1 day'");
    // Same secret, new file content (content the repository has isn't rescanned).
    commit_file(&git, &w, "tests/data/key2.pem", &format!("copy\n{fixture}"));
    assert!(
        git.fails(&w, &["push", &url, "main"])
            .contains("(private-key)")
    );
}

#[test]
fn conf_sec_08_the_allowlist_counts_only_on_the_default_branch() {
    let s = TestServer::start();
    let git = GitClient::new();
    let d = tempfile::tempdir().unwrap();
    let w = d.path().join("w");
    work(&git, &w);
    let url = s.url(&DEMO);
    let (_, fixture) = samples()
        .into_iter()
        .find(|(r, _)| *r == "private-key")
        .unwrap();
    // Added together with the secret: no effect.
    commit_file(
        &git,
        &w,
        ".slath/secret-scan.toml",
        "[[allow]]\npath = \"fixtures/**\"\n",
    );
    commit_file(&git, &w, "fixtures/ssh/id_test", &fixture);
    let err = git.fails(&w, &["push", &url, "main"]);
    assert!(err.contains("fixtures/ssh/id_test"), "{err}");
    // Once it's on the default branch, it counts.
    git.ok(&w, &["reset", "-q", "--hard", "HEAD~1"]);
    git.ok(&w, &["push", "-q", &url, "main"]);
    commit_file(&git, &w, "fixtures/ssh/id_test", &fixture);
    let out = git.cmd(&w).args(["push", &url, "main"]).output().unwrap();
    let text = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{text}");
    assert!(text.contains("allowlist on the default branch"), "{text}");
    // Outside the allowed paths, it still blocks.
    let (_, token) = samples()
        .into_iter()
        .find(|(r, _)| *r == "github-token")
        .unwrap();
    commit_file(&git, &w, "src/deploy.env", &token);
    assert!(
        git.fails(&w, &["push", &url, "main"])
            .contains("src/deploy.env")
    );
}

#[test]
fn conf_sec_08_admins_accept_the_findings_of_a_first_push() {
    use slath_tests::http::submit;
    use slath_tests::server::PASSWORD;
    let s = TestServer::start();
    s.create_user("carol", PASSWORD, false);
    let git = GitClient::new();
    let d = tempfile::tempdir().unwrap();
    let w = d.path().join("w");
    work(&git, &w);
    let (_, a) = samples()
        .into_iter()
        .find(|(r, _)| *r == "github-token")
        .unwrap();
    let (_, b) = samples()
        .into_iter()
        .find(|(r, _)| *r == "url-credentials")
        .unwrap();
    commit_file(&git, &w, "old/config", &format!("{a}{b}"));
    let url = s.url(&DEMO);
    let err = git.fails(&w, &["push", &url, "main"]);
    let line = err
        .lines()
        .find(|l| l.contains("/-/secret-scan/accept/"))
        .unwrap_or_else(|| panic!("{err}"));
    let full = line.split_whitespace().last().unwrap();
    let link = &full[full.find("/alice/").unwrap()..];

    // A writer who isn't an administrator can't accept them.
    let mut alice_admin = s.login("alice", PASSWORD);
    assert_eq!(
        submit(
            &mut alice_admin,
            "/alice/demo/settings/access",
            "/alice/demo/settings/access",
            &[("username", "carol"), ("role", "write")]
        )
        .status,
        303
    );
    let mut carol = s.login("carol", PASSWORD);
    assert_eq!(carol.get(link).status, 404);

    let page = alice_admin.get(link);
    assert_eq!(page.status, 200);
    assert!(page.body.contains("2 possible secret(s)"), "{}", page.body);
    let r = submit(&mut alice_admin, link, link, &[]);
    assert_eq!(r.status, 303, "{}", r.body);
    git.ok(&w, &["push", "-q", &url, "main"]);
    assert_eq!(
        s.db.query_one(
            "SELECT count(*)::text FROM audit_events WHERE action = 'secret_scan.accept_first_push'"
        ),
        "1"
    );
    // A later push of a new secret isn't covered.
    let (_, c) = samples()
        .into_iter()
        .find(|(r, _)| *r == "slack-token")
        .unwrap();
    commit_file(&git, &w, "chat.env", &c);
    let err = git.fails(&w, &["push", &url, "main"]);
    assert!(!err.contains("/-/secret-scan/accept/"), "{err}");
}

#[test]
fn conf_sec_08_rejections_are_audited_and_no_surface_shows_the_secret() {
    let s = TestServer::start();
    let git = GitClient::new();
    let d = tempfile::tempdir().unwrap();
    let w = d.path().join("w");
    work(&git, &w);
    let url = s.url(&DEMO);
    git.ok(&w, &["push", "-q", &url, "main"]);
    let (_, text) = samples()
        .into_iter()
        .find(|(r, _)| *r == "github-token")
        .unwrap();
    let secret = text.trim().trim_start_matches("token: ").to_owned();
    commit_file(&git, &w, "ci.env", &text);
    let err = git.fails(&w, &["push", &url, "main"]);
    assert!(err.contains("(github-token)"), "{err}");
    assert!(
        !err.contains(&secret),
        "the full secret is in the push output"
    );
    assert_eq!(
        s.db.query_one(
            "SELECT count(*)::text FROM audit_events WHERE action = 'push.secret_rejected'"
        ),
        "1"
    );
    let data =
        s.db.query_one("SELECT data::text FROM audit_events WHERE action = 'push.secret_rejected'");
    assert!(
        data.contains("\"rules\": [\"github-token\"]") && data.contains("\"count\": 1"),
        "{data}"
    );
    assert!(data.contains("\"transport\": \"http\""), "{data}");
    let all =
        s.db.query_one("SELECT coalesce(string_agg(data::text, ' '), '') FROM audit_events");
    assert!(!all.contains(&secret) && !all.contains("ci.env"), "{all}");
    assert!(
        !s.log().contains(&secret),
        "the full secret is in the server log"
    );
}

#[test]
fn conf_sec_08_repositories_can_require_admins_to_allow_findings() {
    use slath_tests::http::submit;
    use slath_tests::server::PASSWORD;
    let s = TestServer::start();
    s.create_user("carol", PASSWORD, false);
    let mut alice = s.login("alice", PASSWORD);
    assert_eq!(
        submit(
            &mut alice,
            "/alice/demo/settings/access",
            "/alice/demo/settings/access",
            &[("username", "carol"), ("role", "write")]
        )
        .status,
        303
    );
    let r = submit(
        &mut alice,
        "/alice/demo/settings/general",
        "/alice/demo/settings/push-protection",
        &[("mode", "block"), ("admins_allow", "1")],
    );
    assert_eq!(r.status, 303, "{}", r.body);
    assert!(
        alice
            .get("/alice/demo/settings/general")
            .body
            .contains("name=\"admins_allow\" value=\"1\" checked")
    );
    let fp = "0123456789abcdef0123456789abcdef";
    let link = format!("/alice/demo/-/secret-scan/allow?fingerprint={fp}&rule=github-token");
    let mut carol = s.login("carol", PASSWORD);
    let r = submit(
        &mut carol,
        &link,
        "/alice/demo/-/secret-scan/allow",
        &[("fingerprint", fp), ("rule", "github-token"), ("days", "1")],
    );
    assert!(
        r.status == 403 || r.status == 404,
        "{}: {}",
        r.status,
        r.body
    );
    let r = submit(
        &mut alice,
        &link,
        "/alice/demo/-/secret-scan/allow",
        &[("fingerprint", fp), ("rule", "github-token"), ("days", "1")],
    );
    assert_eq!(r.status, 303, "{}", r.body);
    assert_eq!(
        s.db.query_one("SELECT count(*)::text FROM secret_allowances"),
        "1"
    );
}
