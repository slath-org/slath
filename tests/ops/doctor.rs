// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! `slath doctor` (OPS-11): a healthy instance has no problems, and each
//! injected fault yields its stable code and a fix, in text and in JSON
//! that matches docs/cli/doctor.schema.json.

use std::os::unix::fs::PermissionsExt;

use slath_tests::server::TestServer;

fn doctor(s: &TestServer) -> (bool, serde_json::Value, String) {
    let (ok, out, err) = s.slath(&["doctor", "--output", "json"], None);
    let v: serde_json::Value = serde_json::from_str(&out).unwrap_or_else(|_| panic!("{out}{err}"));
    let schema: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../docs/cli/doctor.schema.json"
        ))
        .unwrap(),
    )
    .unwrap();
    let validator = jsonschema::draft202012::new(&schema).unwrap();
    let errors: Vec<String> = validator.iter_errors(&v).map(|e| e.to_string()).collect();
    assert!(errors.is_empty(), "{errors:?}\n{v}");
    let (_, text, _) = s.slath(&["doctor"], None);
    (ok, v, text)
}

fn status<'a>(v: &'a serde_json::Value, code: &str) -> Vec<&'a str> {
    v["checks"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|c| c["code"] == code)
        .map(|c| c["status"].as_str().unwrap())
        .collect()
}

#[test]
fn conf_ops_11_doctor_finds_each_fault_with_its_code() {
    let s = TestServer::start();
    let (ok, v, text) = doctor(&s);
    assert!(ok, "{v}");
    assert_eq!(v["ok"], true);
    assert!(status(&v, "backup.none").contains(&"warn"), "{v}");
    assert!(text.contains("0 problem(s)"), "{text}");

    // Group and world can read the data directory: the SSH login could.
    // (The test's own root is private too, so open both.)
    let data = s.data_dir();
    let root = s.dir.path().to_path_buf();
    let set = |mode| {
        for p in [&root, &data] {
            std::fs::set_permissions(p, std::fs::Permissions::from_mode(mode)).unwrap();
        }
    };
    set(0o755);
    let (ok, v, text) = doctor(&s);
    set(0o700);
    assert!(!ok);
    assert_eq!(status(&v, "ssh.boundary"), vec!["fail"], "{v}");
    assert!(
        text.contains("FAIL ssh.boundary") && text.contains("fix:"),
        "{text}"
    );

    // The database is newer than this binary.
    s.db.execute("UPDATE schema_info SET version = version + 1");
    let (ok, v, _) = doctor(&s);
    s.db.execute("UPDATE schema_info SET version = version - 1");
    assert!(!ok);
    assert_eq!(status(&v, "schema.state"), vec!["fail"], "{v}");

    // A backup makes backup.none go away.
    let backup = s.dir.path().join("data/backups/manual");
    let (ok, out, err) = s.slath(
        &["backup", "create", "--output", backup.to_str().unwrap()],
        None,
    );
    assert!(ok, "{out}{err}");
    let (_, v, _) = doctor(&s);
    assert_eq!(status(&v, "backup.stale"), vec!["ok"], "{v}");

    // The database can't be reached.
    let cfg = std::fs::read_to_string(s.config_path()).unwrap();
    let broken = cfg.replace(
        &format!(
            "{}/",
            s.db.url
                .split('@')
                .nth(1)
                .unwrap()
                .split('/')
                .next()
                .unwrap()
        ),
        "127.0.0.1:1/",
    );
    assert_ne!(broken, cfg);
    std::fs::write(s.config_path(), &broken).unwrap();
    let (ok, v, _) = doctor(&s);
    std::fs::write(s.config_path(), &cfg).unwrap();
    assert!(!ok);
    assert_eq!(status(&v, "db.unreachable"), vec!["fail"], "{v}");
    // The output never names the database URL (it may hold a password).
    assert!(!v.to_string().contains("postgres://"), "{v}");
}
