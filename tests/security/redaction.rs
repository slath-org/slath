// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Secrets never reach the server log, even at debug level: passwords
//! (right and wrong), tokens, session cookies, CSRF tokens, the database
//! password, and the master key.

use slath_tests::http::{create_token, csrf_token, login, submit};
use slath_tests::server::{DEMO, GitClient, TestServer};

const PW: &str = "correct horse battery staple";
const WRONG: &str = "wrong horse battery staple";
const NEW_PW: &str = "a completely new passphrase";

#[test]
fn conf_sec_04_secrets_never_appear_in_logs() {
    let s = TestServer::start_opts(&[DEMO], "", &[("SLATH_LOG", "debug")]);
    s.create_user("bob", PW, false);
    // Failed and successful sign-ins, through the form.
    let mut c = s.client();
    assert_ne!(login(&mut c, "bob", WRONG).status, 303);
    assert_eq!(login(&mut c, "bob", PW).status, 303);
    let session = c
        .cookies
        .iter()
        .find(|(k, _)| k.contains("session"))
        .map(|(_, v)| v.clone())
        .expect("session cookie");
    let csrf = csrf_token(&c.get("/-/settings/profile").body).unwrap();
    // Tokens: created, used as bearer and as Git password, and misused.
    let token = create_token(&mut c, "ci", &["repo:write", "user:read"]);
    let mut api = s.client();
    api.api("GET", "/api/v1/user", Some(&token), None);
    api.api("GET", "/api/v1/user", Some(&format!("{token}x")), None);
    api.get(&format!(
        "/-/nothing?token={token}&password={}",
        PW.replace(' ', "+")
    ));
    let git = GitClient::new();
    let work = tempfile::tempdir().unwrap();
    let authority = s.base().trim_start_matches("http://").to_owned();
    let out = git
        .cmd(work.path())
        .args([
            "ls-remote",
            &format!("http://bob:{token}@{authority}/alice/demo.git"),
        ])
        .output()
        .unwrap();
    assert!(
        !out.status.success(),
        "bob can't read alice's private repository"
    );
    git.ok(work.path(), &["ls-remote", &s.url(&DEMO)]);
    // Sudo and a password change.
    s.db.execute("UPDATE sessions SET sudo_until = NULL");
    let r = submit(
        &mut c,
        "/-/sudo",
        "/-/sudo",
        &[("password", WRONG), ("next", "/")],
    );
    assert_ne!(r.status, 500);
    let r = submit(
        &mut c,
        "/-/sudo",
        "/-/sudo",
        &[("password", PW), ("next", "/")],
    );
    assert_ne!(r.status, 500);
    let r = submit(
        &mut c,
        "/-/settings/security",
        "/-/settings/security/password",
        &[("new_password", NEW_PW), ("confirm_password", NEW_PW)],
    );
    assert_eq!(r.status, 303, "{}", r.body);

    let log = s.log();
    assert!(
        log.contains("DEBUG") || log.contains("debug"),
        "debug logging is on"
    );
    let db_password =
        s.db.url
            .split("://")
            .nth(1)
            .and_then(|r| r.split('@').next())
            .and_then(|u| u.split(':').nth(1))
            .unwrap()
            .to_owned();
    let master_key = std::fs::read_to_string(s.data_dir().join("instance.key")).unwrap();
    let mut secrets = vec![
        ("password", PW.to_owned()),
        ("wrong password", WRONG.to_owned()),
        ("new password", NEW_PW.to_owned()),
        ("token", token.clone()),
        ("token body", token["slath_pat_".len()..].to_owned()),
        ("alice's token", s.token["slath_pat_".len()..].to_owned()),
        ("session cookie", session),
        ("CSRF token", csrf),
        ("master key", master_key.trim().to_owned()),
    ];
    if db_password.len() >= 4 {
        secrets.push(("database password", format!(":{db_password}@")));
    }
    for (what, secret) in secrets {
        assert!(!secret.is_empty());
        assert!(!log.contains(&secret), "the {what} appears in the log");
    }
    // Spaces may be encoded in logged query strings.
    assert!(!log.contains(&PW.replace(' ', "+")) && !log.contains(&PW.replace(' ', "%20")));
}
