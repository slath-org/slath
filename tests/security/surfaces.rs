// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! The authorization matrix (`crates/core/tests/matrix.csv`) driven through
//! every surface that reads or writes repository content: the web UI, raw
//! files, archives, the API, Git over HTTP, and Git over SSH. Each surface
//! must reach the same decision as the policy for every role, visibility,
//! and repository state.

use std::path::{Path, PathBuf};

use slath_tests::fixtures;
use slath_tests::http::{Client, create_token, submit};
use slath_tests::server::{GitClient, PASSWORD, TestRepo, TestServer};
use slath_tests::sshd::{self, Sshd};

const PROJ: TestRepo = TestRepo {
    namespace: "alice",
    name: "proj",
};

/// (name, role as a collaborator).
const PEOPLE: &[(&str, &str)] = &[
    ("stranger", "none"),
    ("reader", "read"),
    ("writer", "write"),
    ("maintainer", "maintain"),
    ("manager", "admin"),
];

struct Person {
    name: &'static str,
    role: &'static str,
    web: Client,
    token: String,
    key: PathBuf,
}

/// The policy's decision for a session-credential cell: true if allowed.
fn expected(matrix: &str, action: &str, vis: &str, state: &str, subject: &str, role: &str) -> bool {
    let prefix = format!("{action},{vis},{state},{subject},{role},session,");
    let line = matrix
        .lines()
        .find(|l| l.starts_with(&prefix))
        .unwrap_or_else(|| panic!("matrix has no cell {prefix}"));
    line.ends_with(",allow")
}

fn git_ok(git: &GitClient, cwd: &Path, ssh: Option<String>, args: &[&str]) -> bool {
    let mut cmd = git.cmd(cwd);
    if let Some(s) = ssh {
        cmd.env("GIT_SSH_COMMAND", s);
    }
    cmd.args(args).output().unwrap().status.success()
}

#[test]
fn conf_rbac_01_every_surface_agrees_with_the_policy() {
    if sshd::skipped() {
        eprintln!("SLATH_SKIP_SSH_TESTS=1: skipping");
        return;
    }
    let matrix = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../crates/core/tests/matrix.csv"),
    )
    .unwrap();
    let s = TestServer::start_with(&[PROJ]);
    let sshd = Sshd::start(&s);
    let git = GitClient::new();
    let work = tempfile::tempdir().unwrap();
    let src = work.path().join("src");
    fixtures::generate("basic", &src, &[]);
    git.ok(
        &src,
        &["push", "-q", &s.url(&PROJ), "refs/heads/*:refs/heads/*"],
    );

    let mut alice = s.login("alice", PASSWORD);
    let mut people = Vec::new();
    for (name, role) in PEOPLE {
        s.create_user(name, PASSWORD, false);
        if *role != "none" {
            let r = submit(
                &mut alice,
                "/alice/proj/settings/access",
                "/alice/proj/settings/access",
                &[("username", name), ("role", role)],
            );
            assert_eq!(r.status, 303, "{name}: {}", r.body);
        }
        let (key, public) = sshd::keygen(work.path(), name);
        s.add_ssh_key(name, &public);
        let mut web = s.login(name, PASSWORD);
        let token = create_token(&mut web, "probe", &["repo:write", "user:read"]);
        people.push(Person {
            name,
            role,
            web,
            token,
            key,
        });
    }

    let base = s.base();
    let authority = base.trim_start_matches("http://").to_owned();
    let mut failures = Vec::new();
    let mut n = 0;
    // (read allowed, read denied, push allowed, push denied) as observed.
    let mut seen = [0u32; 4];
    for vis in ["public", "private"] {
        for state in ["active", "archived"] {
            s.db.execute(&format!(
                "UPDATE repositories SET visibility = '{vis}', state = '{state}'"
            ));
            let cell = format!("{vis}/{state}");

            // Anonymous visitors: web, raw, archive, API, Git over HTTP.
            let want = expected(&matrix, "repo.read", vis, state, "anon", "none");
            let mut anon = s.client();
            let reads = [
                ("web", anon.get("/alice/proj").status == 200),
                (
                    "raw",
                    anon.get("/alice/proj/raw/main/README.md").status == 200,
                ),
                (
                    "archive",
                    anon.get("/alice/proj/archive/main.tar.gz").status == 200,
                ),
                (
                    "api",
                    Client::new(&base)
                        .api("GET", "/api/v1/repos/alice/proj/tree/main", None, None)
                        .status
                        == 200,
                ),
                (
                    "http",
                    git_ok(
                        &git,
                        work.path(),
                        None,
                        &["ls-remote", &format!("{base}/alice/proj.git")],
                    ),
                ),
            ];
            for (surface, got) in reads {
                if got != want {
                    failures.push(format!(
                        "{cell} anonymous read via {surface}: got {got}, policy {want}"
                    ));
                }
            }
            let want = expected(&matrix, "repo.push", vis, state, "anon", "none");
            n += 1;
            let got = git_ok(
                &git,
                &src,
                None,
                &[
                    "push",
                    "-q",
                    &format!("{base}/alice/proj.git"),
                    &format!("main:refs/heads/anon-{n}"),
                ],
            );
            if got != want {
                failures.push(format!(
                    "{cell} anonymous push via http: got {got}, policy {want}"
                ));
            }

            for p in &mut people {
                let who = format!("{cell} {} ({})", p.name, p.role);
                let http_url = format!("http://{}:{}@{authority}/alice/proj.git", p.name, p.token);
                let ssh = Some(sshd.git_ssh_command(&p.key));
                let ssh_url = sshd.url("alice", "proj");
                let want = expected(&matrix, "repo.read", vis, state, "user", p.role);
                let reads = [
                    ("web", p.web.get("/alice/proj").status == 200),
                    (
                        "raw",
                        p.web.get("/alice/proj/raw/main/README.md").status == 200,
                    ),
                    (
                        "archive",
                        p.web.get("/alice/proj/archive/main.tar.gz").status == 200,
                    ),
                    (
                        "api",
                        Client::new(&base)
                            .api(
                                "GET",
                                "/api/v1/repos/alice/proj/tree/main",
                                Some(&p.token),
                                None,
                            )
                            .status
                            == 200,
                    ),
                    (
                        "http",
                        git_ok(&git, work.path(), None, &["ls-remote", &http_url]),
                    ),
                    (
                        "ssh",
                        git_ok(&git, work.path(), ssh.clone(), &["ls-remote", &ssh_url]),
                    ),
                ];
                for (surface, got) in reads {
                    seen[usize::from(!got)] += 1;
                    if got != want {
                        failures.push(format!(
                            "{who} read via {surface}: got {got}, policy {want}"
                        ));
                    }
                }
                let want = expected(&matrix, "repo.push", vis, state, "user", p.role);
                for (surface, url, ssh) in [("http", &http_url, None), ("ssh", &ssh_url, ssh)] {
                    n += 1;
                    let refspec = format!("main:refs/heads/{}-{n}", p.name);
                    let got = git_ok(&git, &src, ssh, &["push", "-q", url, &refspec]);
                    seen[2 + usize::from(!got)] += 1;
                    if got != want {
                        failures.push(format!(
                            "{who} push via {surface}: got {got}, policy {want}"
                        ));
                    }
                }
            }
        }
    }
    assert!(
        failures.is_empty(),
        "{} disagreements:\n{}",
        failures.len(),
        failures.join("\n")
    );
    assert!(
        seen.iter().all(|c| *c > 0),
        "every outcome occurs: {seen:?}"
    );
}
