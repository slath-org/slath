// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Install conformance (OPS-01) against a running installation: the
//! deploy/compose stack started by `scripts/test-compose.sh`, or a host set
//! up by `slath setup` (tests/ops/setup.rs). The administrator is created
//! with the CLI first.

use slath_tests::http::{Client, create_token, login};
use slath_tests::server::GitClient;

fn env(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| panic!("{name} is not set; run scripts/test-compose.sh"))
}

#[test]
#[ignore = "needs a compose deployment; run scripts/test-compose.sh"]
fn conf_ops_01_compose_install_create_repo_and_clone() {
    install_flow(
        &env("SLATH_COMPOSE_URL"),
        &env("SLATH_COMPOSE_SSH_PORT"),
        &env("SLATH_COMPOSE_ADMIN_PASSWORD"),
    );
}

/// Sign in as `ada`, create a repository through the API, add an SSH key,
/// push over HTTP, and clone over HTTP and SSH.
pub fn install_flow(base: &str, ssh_port: &str, password: &str) {
    let mut c = Client::new(base);
    assert_eq!(login(&mut c, "ada", password).status, 303);
    let token = create_token(&mut c, "install", &["repo:write", "user:read"]);

    // Create the repository through the API.
    let r = c.api(
        "POST",
        "/api/v1/repos",
        Some(&token),
        Some(&serde_json::json!({"namespace": "ada", "name": "project", "visibility": "private"})),
    );
    assert_eq!(r.status, 201, "{}", r.body);

    // An SSH key through the settings page.
    let work = tempfile::tempdir().unwrap();
    let (key, public) = slath_tests::sshd::keygen(work.path(), "id_ed25519");
    let r = slath_tests::http::submit(
        &mut c,
        "/-/settings/keys",
        "/-/settings/keys",
        &[("title", "laptop"), ("key", &public)],
    );
    assert_eq!(r.status, 303, "{}", r.body);

    // Push over HTTP, clone over HTTP and SSH.
    let git = GitClient::new();
    let src = work.path().join("src");
    std::fs::create_dir(&src).unwrap();
    git.ok(&src, &["init", "-q"]);
    std::fs::write(src.join("README.md"), "# project\n").unwrap();
    git.ok(&src, &["add", "."]);
    git.ok(&src, &["commit", "-q", "-m", "first"]);
    let authority = base.trim_start_matches("http://");
    let http_url = format!("http://ada:{token}@{authority}/ada/project.git");
    git.ok(&src, &["push", "-q", &http_url, "HEAD:refs/heads/main"]);
    let head = git.ok(&src, &["rev-parse", "HEAD"]);

    git.ok(work.path(), &["clone", "-q", &http_url, "http-clone"]);
    assert_eq!(
        git.ok(&work.path().join("http-clone"), &["rev-parse", "HEAD"]),
        head
    );

    let host = authority.split(':').next().unwrap();
    let ssh_url = format!("ssh://git@{host}:{ssh_port}/ada/project.git");
    let ssh_cmd = format!(
        "ssh -i {} -o IdentitiesOnly=yes -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o BatchMode=yes",
        key.display()
    );
    let out = git
        .cmd(work.path())
        .env("GIT_SSH_COMMAND", &ssh_cmd)
        .args(["clone", "-q", &ssh_url, "ssh-clone"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "ssh clone: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        git.ok(&work.path().join("ssh-clone"), &["rev-parse", "HEAD"]),
        head
    );

    // The private repository stays private.
    assert_eq!(Client::new(base).get("/ada/project").status, 404);
}
