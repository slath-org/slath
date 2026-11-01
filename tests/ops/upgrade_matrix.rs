// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! The upgrade matrix (OPS-03): for every published release, populate an
//! instance with that release's binary, upgrade it with this one, and check
//! that nothing was lost and everything still works. Also: the old binary
//! refuses the upgraded database with restore guidance, and the
//! pre-upgrade backup restores with the old binary.
//!
//! Needs the old binaries: `just upgrade-matrix` builds one per release tag
//! and passes them as `SLATH_UPGRADE_MATRIX="v0.1.0=/path/slath …"`.

use std::path::{Path, PathBuf};

use slath_tests::fixtures;
use slath_tests::http::{create_repo, create_token, submit};
use slath_tests::server::{DEMO, GitClient, Instance, PASSWORD, TestServer, slath_bin};

/// Entity counts compared before and after.
const COUNTED: &[&str] = &[
    "users",
    "namespaces",
    "organizations",
    "org_members",
    "repositories",
    "repo_collaborators",
    "access_tokens",
    "ssh_keys",
    "git_refs",
    "push_events",
    "ref_updates",
];

fn counts(s: &TestServer) -> Vec<String> {
    COUNTED
        .iter()
        .map(|t| {
            format!(
                "{t}={}",
                s.db.query_one(&format!("SELECT count(*)::text FROM {t}"))
            )
        })
        .collect()
}

/// Every ref of every repository with its object ID, from Git.
fn refs(s: &TestServer, git: &GitClient, work: &Path) -> Vec<String> {
    let mut out = Vec::new();
    for repo in ["alice/demo", "acme/tools"] {
        let url = format!("http://x:{}@{}/{repo}.git", s.token, s.addr);
        let listing = git.ok(work, &["ls-remote", &url]);
        out.extend(listing.lines().map(|l| format!("{repo} {l}")));
    }
    out.sort();
    out
}

/// Populates a running old-release server through its own UI and Git.
fn populate(s: &mut TestServer, git: &GitClient, work: &Path) {
    s.create_user("bob", PASSWORD, false);
    s.create_user("root-admin", PASSWORD, true);
    let src = work.join("src");
    fixtures::generate("tags", &src, &[]);
    git.ok(
        &src,
        &[
            "push",
            "-q",
            &s.url(&DEMO),
            "refs/heads/*:refs/heads/*",
            "refs/tags/*:refs/tags/*",
        ],
    );
    let mut a = s.login("alice", PASSWORD);
    assert_eq!(
        submit(
            &mut a,
            "/-/orgs/new",
            "/-/orgs",
            &[("name", "acme"), ("display_name", "Acme")]
        )
        .status,
        303
    );
    assert_eq!(
        submit(
            &mut a,
            "/acme/-/members",
            "/acme/-/members",
            &[("username", "bob"), ("role", "member")]
        )
        .status,
        303
    );
    create_repo(&mut a, "acme", "tools", "public");
    let tools = format!("http://x:{}@{}/acme/tools.git", s.token, s.addr);
    git.ok(&src, &["push", "-q", &tools, "refs/heads/*:refs/heads/*"]);
    assert_eq!(
        submit(
            &mut a,
            "/alice/demo/settings/access",
            "/alice/demo/settings/access",
            &[("username", "bob"), ("role", "write")]
        )
        .status,
        303
    );
    let mut b = s.login("bob", PASSWORD);
    create_token(&mut b, "ci", &["repo:read"]);
}

/// An instance whose configuration every release understands (no keys
/// added after 0.1.0).
fn old_instance() -> Instance {
    let inst = Instance::new("");
    let cfg = std::fs::read_to_string(inst.config_path()).unwrap();
    let cfg: String = cfg
        .lines()
        .filter(|l| !l.starts_with("socket_group"))
        .map(|l| format!("{l}\n"))
        .collect();
    std::fs::write(inst.config_path(), cfg).unwrap();
    inst
}

fn matrix() -> Vec<(String, PathBuf)> {
    let Ok(spec) = std::env::var("SLATH_UPGRADE_MATRIX") else {
        panic!("set SLATH_UPGRADE_MATRIX (run `just upgrade-matrix`)");
    };
    spec.split_whitespace()
        .map(|e| {
            let (tag, bin) = e.split_once('=').unwrap();
            (tag.to_owned(), PathBuf::from(bin))
        })
        .collect()
}

#[test]
#[ignore = "needs every release's binary; run with `just upgrade-matrix`"]
fn conf_ops_03_upgrade_from_every_release() {
    let releases = matrix();
    assert!(!releases.is_empty());
    for (tag, old_bin) in releases {
        eprintln!("== {tag} -> HEAD");
        let mut s = old_instance().start_bin(&old_bin, &[]);
        s.seed_repos(&[DEMO]);
        let git = GitClient::new();
        let work = tempfile::tempdir().unwrap();
        populate(&mut s, &git, work.path());
        let before_counts = counts(&s);
        let before_refs = refs(&s, &git, work.path());
        let old_schema = s.db.query_one("SELECT version::text FROM schema_info");

        // Upgrade with this binary while the old server is stopped.
        s.stop();
        let backups = s.dir.path().join("backups");
        let (ok, out, err) = slath_tests::server::run_slath(
            &s.config_path(),
            &[
                "upgrade",
                "--yes",
                "--backup-dir",
                backups.to_str().unwrap(),
                "--output",
                "json",
            ],
            None,
        );
        assert!(ok, "{tag}: upgrade failed: {out}{err}");
        let v: serde_json::Value = serde_json::from_str(out.lines().last().unwrap()).unwrap();
        let backup = v["backup"].as_str().unwrap().to_owned();

        // Smoke and conformance subset on the upgraded instance.
        s.start_again(&slath_bin(), false)
            .unwrap_or_else(|log| panic!("{tag}: {log}"));
        assert_eq!(counts(&s), before_counts, "{tag}: entity counts");
        assert_eq!(refs(&s, &git, work.path()), before_refs, "{tag}: refs");
        let mut bob = s.login("bob", PASSWORD);
        assert_eq!(
            bob.get("/alice/demo").status,
            200,
            "{tag}: collaborator access"
        );
        assert_eq!(bob.get("/acme/tools").status, 200);
        let mut admin = s.login("root-admin", PASSWORD);
        assert!(admin.get("/-/admin/upgrades").body.contains("success"));
        git.ok(
            work.path(),
            &[
                "clone",
                "-q",
                &format!("http://x:{}@{}/alice/demo.git", s.token, s.addr),
                &format!("clone-{tag}"),
            ],
        );
        let (ok, out, _) = s.slath(&["fsck", "--output", "json"], None);
        assert!(ok, "{tag}: fsck after upgrade: {out}");

        // The old binary refuses the upgraded database and says how to go back.
        let refused = s.start_again(&old_bin, false);
        let log = refused.expect_err("the old binary must not serve the newer schema");
        assert!(log.contains("newer than this binary"), "{tag}: {log}");
        assert!(
            log.contains("restore") || log.contains("Restore"),
            "{tag}: {log}"
        );

        // The pre-upgrade backup restores with the old binary.
        let fresh = old_instance();
        let (ok, out, err) = slath_tests::server::run_slath_bin(
            &old_bin,
            &fresh.config_path(),
            &["backup", "restore", "--input", &backup],
            None,
        );
        assert!(ok, "{tag}: restore with the old binary: {out}{err}");
        assert_eq!(
            fresh.db.query_one("SELECT version::text FROM schema_info"),
            old_schema,
            "{tag}"
        );
        let restored = fresh.start_bin(&old_bin, &[]);
        let mut a = restored.login("alice", PASSWORD);
        assert_eq!(a.get("/alice/demo").status, 200, "{tag}: restored instance");
    }
}
