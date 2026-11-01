// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! SSH key management.

use slath_tests::http::{Client, submit};
use slath_tests::server::TestServer;

const PW: &str = "correct horse battery staple";

fn key(name: &str) -> String {
    let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../crates/auth/tests/data/ssh")
        .join(name);
    std::fs::read_to_string(p).unwrap().trim().to_owned()
}

fn add(c: &mut Client, title: &str, k: &str) -> slath_tests::http::Resp {
    submit(
        c,
        "/-/settings/keys",
        "/-/settings/keys",
        &[("title", title), ("key", k)],
    )
}

#[test]
fn conf_git_02_keys_are_validated_with_clear_errors() {
    let s = TestServer::start_with(&[]);
    s.create_user("alice", PW, false);
    let mut a = s.login("alice", PW);
    let cases = [
        (
            "rsa.pub",
            "RSA keys need at least 3072 bits; this one has 2048.",
        ),
        ("rsa3072-cert.pub", "Certificates aren"),
    ];
    for (file, msg) in cases {
        let r = add(&mut a, "x", &key(file));
        assert_eq!(r.status, 422, "{file}");
        assert!(r.body.contains(msg), "{file}: {}", r.body);
    }
    let r = add(&mut a, "x", "ssh-dss AAAAB3NzaC1kc3MAAACBAP");
    assert!(r.body.contains("DSA keys are insecure"));
    let r = add(&mut a, "x", "ssh-ed25519 not-base64!");
    assert_eq!(r.status, 422);
    let r = add(&mut a, "x", "ssh-ed25519 AAAA");
    assert!(r.body.contains("damaged"));
    let r = add(&mut a, "x", "-----BEGIN OPENSSH PRIVATE KEY-----\nabc");
    assert!(r.body.contains("private key"));
    assert_eq!(s.db.query_one("SELECT count(*)::text FROM ssh_keys"), "0");
    // Good keys are accepted; the title defaults to the comment.
    for f in ["ed25519.pub", "rsa3072.pub", "ecdsa.pub", "ecdsa521.pub"] {
        assert_eq!(add(&mut a, "", &key(f)).status, 303, "{f}");
    }
    let page = a.get("/-/settings/keys");
    assert!(page.body.contains("test-ed25519"));
    assert!(page.body.contains(&key("ed25519.fp")));
}

#[test]
fn conf_git_02_duplicate_fingerprint_across_users_is_rejected_without_naming_the_owner() {
    let s = TestServer::start_with(&[]);
    s.create_user("alice", PW, false);
    s.create_user("mallory", PW, false);
    let mut a = s.login("alice", PW);
    assert_eq!(add(&mut a, "laptop", &key("ed25519.pub")).status, 303);
    let mut m = s.login("mallory", PW);
    // Same key, different comment and whitespace: same fingerprint.
    let k = key("ed25519.pub");
    let mut parts = k.split_whitespace();
    let respaced = format!(
        "{}   {} other-comment",
        parts.next().unwrap(),
        parts.next().unwrap()
    );
    let r = add(&mut m, "stolen", &respaced);
    assert_eq!(r.status, 422);
    assert!(r.body.contains("This key is already in use."));
    assert!(!r.body.contains("alice"), "the owner must not be revealed");
    // The owner adding it twice gets the same answer.
    assert_eq!(add(&mut a, "again", &k).status, 422);
}

#[test]
fn conf_rbac_02_other_users_keys_are_not_found() {
    let s = TestServer::start_with(&[]);
    s.create_user("alice", PW, false);
    s.create_user("mallory", PW, false);
    let mut a = s.login("alice", PW);
    add(&mut a, "laptop", &key("ed25519.pub"));
    let page = a.get("/-/settings/keys");
    let id = page.body.split("/-/settings/keys/").nth(1).unwrap()[..36].to_owned();
    let mut m = s.login("mallory", PW);
    let r = submit(
        &mut m,
        "/-/settings/keys",
        &format!("/-/settings/keys/{id}/delete"),
        &[],
    );
    assert_eq!(r.status, 404);
    assert_eq!(s.db.query_one("SELECT count(*)::text FROM ssh_keys"), "1");
    let r = submit(
        &mut a,
        "/-/settings/keys",
        &format!("/-/settings/keys/{id}/delete"),
        &[],
    );
    assert_eq!(r.status, 303);
    assert_eq!(s.db.query_one("SELECT count(*)::text FROM ssh_keys"), "0");
}

#[test]
fn conf_auth_03_adding_and_removing_keys_needs_sudo() {
    let s = TestServer::start_with(&[]);
    s.create_user("alice", PW, false);
    let mut a = s.login("alice", PW);
    add(&mut a, "laptop", &key("ed25519.pub"));
    s.db.execute("UPDATE sessions SET sudo_until = NULL");
    let r = add(&mut a, "desk", &key("ecdsa.pub"));
    assert_eq!(r.status, 303);
    assert!(r.header("location").unwrap().starts_with("/-/sudo?next="));
    let page = a.get("/-/settings/keys");
    let id = page.body.split("/-/settings/keys/").nth(1).unwrap()[..36].to_owned();
    let r = submit(
        &mut a,
        "/-/settings/keys",
        &format!("/-/settings/keys/{id}/delete"),
        &[],
    );
    assert!(r.header("location").unwrap().starts_with("/-/sudo?next="));
    assert_eq!(s.db.query_one("SELECT count(*)::text FROM ssh_keys"), "1");
}
