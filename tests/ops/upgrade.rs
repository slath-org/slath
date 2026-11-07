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

/// An instance holding a fixture's data, not running.
fn fixture_instance(path: &Path) -> Instance {
    let inst = Instance::new("");
    schema::load(&inst.db.url, path);
    let data = inst.dir.path().join("data");
    std::fs::create_dir_all(data.join("git-home")).unwrap();
    schema::create_repo_dirs(&inst.db, &data);
    inst
}

/// The oldest supported database: data at the upgrade baseline.
fn baseline_instance() -> Instance {
    fixture_instance(&schema::baseline())
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

fn required_schema_of(srv: &slath_tests::server::TestServer) -> i32 {
    let (ok, out, _) = srv.slath(&["version", "--json"], None);
    assert!(ok);
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    i32::try_from(v["schema"].as_i64().unwrap()).unwrap()
}

#[test]
fn conf_ops_07_check_changes_nothing() {
    let inst = baseline_instance();
    let before = (db_checksum(&inst.db), tree_checksum(inst.dir.path()));
    let (ok, out, err) = inst.slath(&["upgrade", "--check", "--output", "json"], None);
    assert!(ok, "{out}{err}");
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["ok"], true);
    assert_eq!(v["preflight"]["from_version"], serde_json::Value::Null);
    assert_eq!(v["preflight"]["from_schema"], 100);
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
    let inst = baseline_instance();
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
        let inst = baseline_instance();
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
        // The re-run finished the interrupted upgrade, even when the schema
        // was already current, and cleared its marker.
        assert_eq!(
            inst.db.query_one(
                "SELECT count(*)::text FROM instance_settings WHERE key = 'upgrade_in_progress'"
            ),
            "0",
            "{step}"
        );
        // Running it once more is a no-op.
        let (ok, out, _) = inst.slath(&["upgrade", "--yes"], None);
        assert!(ok && out.contains("Already up to date"), "{step}: {out}");
    }
}

#[test]
fn conf_ops_07_upgrade_then_restore_the_pre_upgrade_backup() {
    let inst = baseline_instance();
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
    assert!(backup.contains("pre-upgrade-schema100-"), "{backup}");
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
        page.body.contains("schema 100") && page.body.contains("success"),
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

/// The baseline data and every published release's data migrate to the
/// current schema.
#[test]
fn migrations_apply_to_every_release_fixture() {
    let fixtures =
        std::iter::once(("baseline".to_owned(), schema::baseline())).chain(schema::releases());
    for (version, path) in fixtures {
        let inst = fixture_instance(&path);
        let (ok, out, err) = inst.slath(&["migrate"], None);
        assert!(ok, "{version}: {out}{err}");
        assert_eq!(
            schema_version(&inst.db),
            required_schema(&inst),
            "{version}"
        );
    }
}

/// A preview's database is refused everywhere with guidance, and nothing
/// changes.
#[test]
fn conf_ops_03_preview_databases_are_refused_with_guidance() {
    let inst = fixture_instance(&schema::preview("0.2.0"));
    let before = db_checksum(&inst.db);
    for args in [
        &["upgrade", "--yes"][..],
        &["upgrade", "--check"],
        &["migrate"],
        &["serve"],
    ] {
        let (ok, out, err) = inst.slath(args, None);
        assert!(!ok, "{args:?}: {out}{err}");
        let text = format!("{out}{err}");
        assert!(text.contains("preview"), "{args:?}: {text}");
        assert!(text.contains("Import a repository"), "{args:?}: {text}");
    }
    let (ok, out, err) = inst.slath(&["serve"], None);
    assert!(!ok);
    assert!(
        format!("{out}{err}").contains("preview release"),
        "{out}{err}"
    );
    assert_eq!(db_checksum(&inst.db), before);
    assert_eq!(schema_version(&inst.db), 24);
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

/// Updating by restart (OPS-10): starting the new binary on an older
/// database runs the preflight, takes the pre-upgrade backup, keeps the
/// newest two, and migrates.
#[test]
fn conf_ops_10_restarting_with_a_new_version_backs_up_and_migrates() {
    let inst = baseline_instance();
    // Three older backups from earlier updates.
    let backups = inst.dir.path().join("data/backups");
    for (i, name) in ["pre-upgrade-a", "pre-upgrade-b", "pre-upgrade-c"]
        .iter()
        .enumerate()
    {
        let d = backups.join(name);
        std::fs::create_dir_all(&d).unwrap();
        let manifest = d.join("manifest.json");
        std::fs::write(&manifest, "{}").unwrap();
        let t =
            std::time::SystemTime::now() - std::time::Duration::from_secs(3600 * (3 - i as u64));
        std::fs::File::options()
            .write(true)
            .open(&manifest)
            .unwrap()
            .set_modified(t)
            .unwrap();
    }
    std::fs::create_dir_all(backups.join("unrelated")).unwrap();
    let srv = inst.start(&[]);
    assert_eq!(schema_version(&srv.db), required_schema_of(&srv));
    assert_eq!(
        srv.db.query_one("SELECT outcome FROM upgrade_history"),
        "success"
    );
    let backup = srv.db.query_one("SELECT backup_path FROM upgrade_history");
    assert!(
        backup.contains("/backups/pre-upgrade-schema100-"),
        "{backup}"
    );
    let mut left: Vec<String> = std::fs::read_dir(&backups)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    left.sort();
    let new = std::path::Path::new(&backup)
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    assert_eq!(
        left,
        vec!["pre-upgrade-c".to_owned(), new, "unrelated".to_owned()]
    );
    let mut c = srv.client();
    assert_eq!(
        slath_tests::http::login(&mut c, "alice", PASSWORD).status,
        303
    );
    assert!(srv.log().contains("update applied"), "{}", srv.log());
}

/// A failed preflight refuses to start and changes nothing.
#[test]
fn conf_ops_10_a_failed_preflight_refuses_to_start() {
    use std::os::unix::fs::PermissionsExt;
    let inst = baseline_instance();
    let data = inst.dir.path().join("data");
    // An installation that ran before has its hooks directory.
    std::fs::create_dir_all(data.join("hooks")).unwrap();
    let before = db_checksum(&inst.db);
    std::fs::set_permissions(&data, std::fs::Permissions::from_mode(0o500)).unwrap();
    let (ok, out, err) = inst.slath(&["serve"], None);
    std::fs::set_permissions(&data, std::fs::Permissions::from_mode(0o700)).unwrap();
    assert!(!ok);
    let text = format!("{out}{err}");
    assert!(text.contains("refusing to start"), "{text}");
    assert!(text.contains("data_dir"), "{text}");
    assert!(text.contains("Nothing was changed"), "{text}");
    assert_eq!(db_checksum(&inst.db), before);
    assert_eq!(schema_version(&inst.db), 100);
}

/// With `auto_backup = false`, a restart migrates without a backup.
#[test]
fn restart_updates_skip_the_backup_when_turned_off() {
    let inst = fixture_instance(&schema::baseline());
    let cfg = std::fs::read_to_string(inst.config_path()).unwrap();
    std::fs::write(
        inst.config_path(),
        format!("{cfg}[upgrade]\nauto_backup = false\n"),
    )
    .unwrap();
    let srv = inst.start(&[]);
    assert_eq!(schema_version(&srv.db), required_schema_of(&srv));
    assert_eq!(
        srv.db
            .query_one("SELECT coalesce(backup_path, 'none') FROM upgrade_history"),
        "none"
    );
    assert!(!srv.data_dir().join("backups").exists());
}
