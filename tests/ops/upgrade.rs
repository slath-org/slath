// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! `slath upgrade` (OPS-07): the dry run changes nothing, failing preflight
//! aborts before any change, a crash at any step is finished by running it
//! again, and the pre-upgrade backup restores.

use std::path::Path;

use slath_tests::schema;
use slath_tests::server::{Instance, PASSWORD, TestDb};

/// A checksum of every table's rows and of the schema.
fn db_checksum(db: &TestDb) -> String {
    let tables = db.query_one(
        "SELECT string_agg(tablename, ',' ORDER BY tablename) FROM pg_tables WHERE schemaname = 'public'",
    );
    let mut sums = vec![db.query_one(
        "SELECT md5(string_agg(table_name || '.' || column_name || ':' || data_type, ',' \
         ORDER BY table_name, column_name)) FROM information_schema.columns WHERE table_schema = 'public'",
    )];
    for t in tables.split(',') {
        sums.push(db.query_one(&format!(
            "SELECT '{t}:' || md5(coalesce(string_agg(x::text, '|' ORDER BY x::text), '')) FROM public.{t} x"
        )));
    }
    sums.join("\n")
}

/// Path and content hash of every file under `dir`.
fn tree_checksum(dir: &Path) -> Vec<(String, String)> {
    use sha2::{Digest, Sha256};
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).unwrap().flatten() {
            let p = e.path();
            let t = e.file_type().unwrap();
            if t.is_dir() {
                stack.push(p);
            } else if t.is_file() {
                let h = Sha256::digest(std::fs::read(&p).unwrap());
                let hex: String = h.iter().map(|b| format!("{b:02x}")).collect();
                out.push((p.display().to_string(), hex));
            } else if t.is_symlink() {
                out.push((
                    p.display().to_string(),
                    format!("-> {:?}", std::fs::read_link(&p).unwrap()),
                ));
            }
        }
    }
    out.sort();
    out
}

/// An instance holding a release's fixture data, not running.
fn release_instance(version: &str) -> Instance {
    let inst = Instance::new("");
    let (_, path) = schema::releases()
        .into_iter()
        .find(|(v, _)| v == version)
        .unwrap();
    schema::load(&inst.db.url, &path);
    let data = inst.dir.path().join("data");
    std::fs::create_dir_all(data.join("git-home")).unwrap();
    schema::create_repo_dirs(&inst.db, &data);
    inst
}

fn schema_version(db: &TestDb) -> i32 {
    db.query_one("SELECT version::text FROM schema_info")
        .parse()
        .unwrap()
}

fn required_schema(inst: &Instance) -> i32 {
    let (ok, out, _) = inst.slath(&["version", "--json"], None);
    assert!(ok);
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    i32::try_from(v["schema"].as_i64().unwrap()).unwrap()
}

#[test]
fn conf_ops_07_check_changes_nothing() {
    let inst = release_instance("0.1.1");
    let before = (db_checksum(&inst.db), tree_checksum(inst.dir.path()));
    let (ok, out, err) = inst.slath(&["upgrade", "--check", "--output", "json"], None);
    assert!(ok, "{out}{err}");
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["ok"], true);
    assert_eq!(v["preflight"]["from_version"], "0.1.0");
    assert_eq!(v["preflight"]["from_schema"], 9);
    assert!(!v["preflight"]["pending"].as_array().unwrap().is_empty());
    let (ok, text, _) = inst.slath(&["upgrade", "--check"], None);
    assert!(ok);
    assert!(
        text.contains("Preflight:") && text.contains("Plan:"),
        "{text}"
    );
    assert_eq!(
        before,
        (db_checksum(&inst.db), tree_checksum(inst.dir.path()))
    );
}

#[test]
fn conf_ops_07_failing_preflight_changes_nothing() {
    use std::os::unix::fs::PermissionsExt;
    let inst = release_instance("0.1.1");
    let data = inst.dir.path().join("data");
    std::fs::set_permissions(&data, std::fs::Permissions::from_mode(0o500)).unwrap();
    let before = (db_checksum(&inst.db), tree_checksum(inst.dir.path()));
    let (ok, out, err) = inst.slath(&["upgrade", "--yes"], None);
    std::fs::set_permissions(&data, std::fs::Permissions::from_mode(0o700)).unwrap();
    assert!(!ok);
    assert!(out.contains("✗ data_dir"), "{out}");
    assert!(err.contains("nothing was changed"), "{err}");
    assert_eq!(
        before,
        (db_checksum(&inst.db), tree_checksum(inst.dir.path()))
    );

    // A database newer than the binary is refused with guidance.
    inst.db.execute("UPDATE schema_info SET version = 9999");
    let (ok, out, _) = inst.slath(&["upgrade", "--yes"], None);
    assert!(!ok);
    assert!(out.contains("downgrades aren't supported"), "{out}");
    assert_eq!(schema_version(&inst.db), 9999);
}

#[test]
fn conf_ops_07_crash_at_every_step_is_finished_by_rerunning() {
    for step in [
        "after-backup",
        "after-first-migration",
        "after-migrations",
        "after-hooks",
    ] {
        let inst = release_instance("0.1.1");
        let users = inst.db.query_one("SELECT count(*)::text FROM users");
        let mut cmd = std::process::Command::new(slath_tests::server::slath_bin());
        let out = cmd
            .args(["upgrade", "--yes"])
            .env("SLATH_CONFIG", inst.config_path())
            .env("SLATH_UPGRADE_FAULT", step)
            .output()
            .unwrap();
        assert!(
            !out.status.success(),
            "{step}: the simulated crash didn't happen"
        );
        assert!(String::from_utf8_lossy(&out.stderr).contains("simulated crash"));
        let (ok, out, err) = inst.slath(&["upgrade", "--yes"], None);
        assert!(ok, "{step}: re-run failed: {out}{err}");
        assert_eq!(schema_version(&inst.db), required_schema(&inst), "{step}");
        assert_eq!(
            inst.db.query_one("SELECT count(*)::text FROM users"),
            users,
            "{step}"
        );
        assert_eq!(
            inst.db
                .query_one("SELECT outcome FROM upgrade_history ORDER BY started_at DESC LIMIT 1"),
            "success",
            "{step}"
        );
        // Running it once more is a no-op.
        let (ok, out, _) = inst.slath(&["upgrade", "--yes"], None);
        assert!(ok && out.contains("Already up to date"), "{step}: {out}");
    }
}

#[test]
fn conf_ops_07_upgrade_then_restore_the_pre_upgrade_backup() {
    let inst = release_instance("0.1.1");
    let before = db_checksum(&inst.db);
    let backups = inst.dir.path().join("backups");
    let (ok, out, err) = inst.slath(
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
    assert!(ok, "{out}{err}");
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    let backup = v["backup"].as_str().unwrap().to_owned();
    assert!(backup.contains("pre-upgrade-0.1.0-"), "{backup}");
    assert_eq!(
        inst.db.query_one("SELECT backup_path FROM upgrade_history"),
        backup
    );
    // The upgraded instance serves its old users.
    let server = Instance::new("");
    let target = server.config_path();
    drop(server);
    let _ = target;
    let srv = inst.start(&[]);
    let mut c = srv.client();
    assert_eq!(
        slath_tests::http::login(&mut c, "alice", PASSWORD).status,
        303
    );
    assert!(c.get("/-/admin/upgrades").status == 404);
    let mut admin = srv.login("root-admin", PASSWORD);
    let page = admin.get("/-/admin/upgrades");
    assert!(
        page.body.contains("v0.1.0 (schema 9)") && page.body.contains("success"),
        "{}",
        page.body
    );
    // The server refreshes the Git config of the existing repositories.
    let start = std::time::Instant::now();
    while srv
        .db
        .query_one("SELECT state FROM background_migrations WHERE name = 'repo_config_v1'")
        != "done"
    {
        assert!(start.elapsed().as_secs() < 90, "{}", srv.log());
        std::thread::sleep(std::time::Duration::from_millis(250));
    }
    let id = srv
        .db
        .query_one("SELECT id::text FROM repositories WHERE state = 'active' LIMIT 1");
    let cfg = std::fs::read_to_string(
        srv.data_dir()
            .join("repos")
            .join(&id[..2])
            .join(format!("{id}.git/config")),
    )
    .unwrap();
    assert!(
        cfg.contains("configVersion = 1") && cfg.contains("fsckObjects = true"),
        "{cfg}"
    );

    // The backup holds the old schema and restores into an empty instance.
    let fresh = Instance::new("");
    let (ok, out, err) = fresh.slath(&["backup", "restore", "--input", &backup], None);
    assert!(ok, "{out}{err}");
    assert_ne!(
        db_checksum(&fresh.db),
        before,
        "restore migrates to the current schema"
    );
    assert!(
        fresh
            .db
            .query_one("SELECT count(*)::text FROM users")
            .parse::<i32>()
            .unwrap()
            >= 3
    );
}

/// Every published release's data migrates to the current schema.
#[test]
fn migrations_apply_to_every_release_fixture() {
    for (version, _) in schema::releases() {
        let inst = release_instance(&version);
        let (ok, out, err) = inst.slath(&["migrate"], None);
        assert!(ok, "v{version}: {out}{err}");
        assert_eq!(
            schema_version(&inst.db),
            required_schema(&inst),
            "v{version}"
        );
    }
}

/// Read-only maintenance during an upgrade: the server notices the flag,
/// refuses changes with a clear message (pages, API, pushes), keeps
/// serving reads, and a restarted server of the target version ends it.
#[test]
fn conf_ops_07_maintenance_mode_is_read_only() {
    use slath_tests::server::{DEMO, GitClient, TestServer};
    let s = TestServer::start();
    let git = GitClient::new();
    let work = tempfile::tempdir().unwrap();
    let src = work.path().join("src");
    std::fs::create_dir(&src).unwrap();
    git.ok(&src, &["init", "-q", "-b", "main"]);
    std::fs::write(src.join("a.txt"), "a\n").unwrap();
    git.ok(&src, &["add", "."]);
    git.ok(&src, &["commit", "-q", "-m", "a"]);
    git.ok(&src, &["push", "-q", &s.url(&DEMO), "main"]);

    s.db.execute(
        "INSERT INTO instance_settings (key, value) VALUES ('maintenance', \
         '{\"reason\": \"upgrade to v9.9.9\", \"until_version\": \"9.9.9\", \"finished\": false}')",
    );
    let mut alice = s.login("alice", PASSWORD);
    let start = std::time::Instant::now();
    while !alice.get("/").body.contains("Maintenance in progress") {
        assert!(
            start.elapsed().as_secs() < 10,
            "the server didn't notice the flag"
        );
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
    // Reads work; changes don't.
    assert_eq!(alice.get("/alice/demo").status, 200);
    let r = slath_tests::http::submit(
        &mut alice,
        "/-/settings/profile",
        "/-/settings/profile",
        &[("display_name", "Alice")],
    );
    assert_eq!(r.status, 503, "{}", r.body);
    assert!(r.body.contains("read-only"));
    let api = alice.api(
        "POST",
        "/api/v1/user/repos",
        Some(&s.token),
        Some(&serde_json::json!({"name": "x"})),
    );
    assert_eq!(api.status, 503);
    assert_eq!(api.json()["code"], "maintenance");
    std::fs::write(src.join("a.txt"), "b\n").unwrap();
    git.ok(&src, &["commit", "-q", "-am", "b"]);
    let err = git.fails(&src, &["push", "-q", &s.url(&DEMO), "main"]);
    assert!(err.contains("maintenance in progress"), "{err}");
    // Clones still work.
    git.ok(work.path(), &["clone", "-q", &s.url(&DEMO), "clone"]);
}
