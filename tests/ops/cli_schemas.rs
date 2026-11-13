// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Every `slath admin` command's `--output json` (OPS-08) matches its
//! schema in docs/cli/, on success and on failure.

use serde_json::Value;
use slath_tests::server::{PASSWORD, TestServer};

fn validate(command: &str, out: &str) -> Value {
    let v: Value =
        serde_json::from_str(out).unwrap_or_else(|e| panic!("{command}: not JSON ({e}): {out}"));
    let path = format!(
        "{}/../docs/cli/{command}.schema.json",
        env!("CARGO_MANIFEST_DIR")
    );
    let schema: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    let validator = jsonschema::draft202012::new(&schema).unwrap();
    let errors: Vec<String> = validator.iter_errors(&v).map(|e| e.to_string()).collect();
    assert!(errors.is_empty(), "{command}: {errors:?}\n{v}");
    v
}

/// Runs a command with `--output json` and checks its output; returns it
/// and whether it succeeded.
fn run(s: &TestServer, schema: &str, args: &[&str], stdin: Option<&str>) -> (bool, Value) {
    let mut all = args.to_vec();
    all.extend(["--output", "json"]);
    let (ok, out, _) = s.slath(&all, stdin);
    let v = validate(schema, &out);
    assert_eq!(
        v["ok"] == true,
        ok,
        "{schema}: exit status and ok disagree: {v}"
    );
    (ok, v)
}

#[test]
fn conf_ops_08_admin_commands_print_schema_valid_json() {
    let smtp = slath_tests::smtp::SmtpServer::start();
    let s = TestServer::start_full(&[], &smtp.config());
    let dir = tempfile::tempdir().unwrap();

    let create_user = [
        "admin",
        "create-user",
        "--username",
        "ada",
        "--email",
        "ada@example.org",
    ];
    assert!(run(&s, "admin-create-user", &create_user, None).0);
    assert!(!run(&s, "admin-create-user", &create_user, None).0);
    let mut with_pw = create_user.to_vec();
    with_pw[3] = "bea";
    with_pw[5] = "bea@example.org";
    with_pw.push("--password-stdin");
    let (_, v) = run(&s, "admin-create-user", &with_pw, Some(PASSWORD));
    assert_eq!(v["password"], Value::Null);

    let bot = ["admin", "create-bot", "--owner", "ada", "--name", "ada-ci"];
    assert!(run(&s, "admin-create-bot", &bot, None).0);
    assert!(!run(&s, "admin-create-bot", &bot, None).0);

    let seed = dir.path().join("seed.toml");
    std::fs::write(
        &seed,
        "[[repos]]\nnamespace = \"ada\"\nname = \"app\"\n\n[[tokens]]\nname = \"ci\"\nbot = \"ada-ci\"\nscopes = [\"repo:read\"]\nrepos = [\"ada/app\"]\nfile = \"ci-token\"\n",
    )
    .unwrap();
    let seed = seed.to_str().unwrap();
    assert!(run(&s, "admin-apply", &["admin", "apply", seed], None).0);
    let (_, v) = run(&s, "admin-apply", &["admin", "apply", seed], None);
    assert_eq!(v["changes"], serde_json::json!([]));
    assert!(
        !run(
            &s,
            "admin-apply",
            &["admin", "apply", "/nonexistent.toml"],
            None
        )
        .0
    );

    for (args, changed) in [
        (["admin", "bot", "stop", "ada-ci"], true),
        (["admin", "bot", "stop", "ada-ci"], false),
        (["admin", "bot", "start", "ada-ci"], true),
        (["admin", "bot", "start", "ada-ci"], false),
    ] {
        let (ok, v) = run(&s, "admin-bot", &args, None);
        assert!(ok, "{v}");
        assert_eq!(v["changed"], changed, "{args:?}: {v}");
    }
    assert!(!run(&s, "admin-bot", &["admin", "bot", "stop", "nobody"], None).0);
    for (args, changed) in [
        (&["admin", "bot", "freeze"][..], true),
        (&["admin", "bot", "freeze"][..], false),
        (&["admin", "bot", "unfreeze"][..], true),
    ] {
        let (ok, v) = run(&s, "admin-bot", args, None);
        assert!(ok, "{v}");
        assert_eq!(v["changed"], changed, "{args:?}: {v}");
    }
    assert!(
        !run(
            &s,
            "admin-bot",
            &["admin", "bot", "freeze", "--org", "nope"],
            None
        )
        .0
    );

    let reset = ["admin", "reset-mfa", "--username"];
    run(
        &s,
        "admin-reset-mfa",
        &[&reset[..], &["ada"]].concat(),
        None,
    );
    assert!(
        !run(
            &s,
            "admin-reset-mfa",
            &[&reset[..], &["nobody"]].concat(),
            None
        )
        .0
    );

    let mail = ["admin", "send-test-email", "--to", "ada@example.org"];
    assert!(run(&s, "admin-send-test-email", &mail, None).0);

    assert!(run(&s, "admin-rotate-key", &["admin", "rotate-key"], None).0);

    let bg = ["admin", "background-migrations"];
    assert!(
        run(
            &s,
            "admin-background-migrations",
            &[&bg[..], &["list"]].concat(),
            None
        )
        .0
    );
    assert!(
        !run(
            &s,
            "admin-background-migrations",
            &[&bg[..], &["pause", "no-such-migration"]].concat(),
            None
        )
        .0
    );
}
