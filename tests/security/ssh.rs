// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! The SSH key lookup (`slath ssh-keys`) against the real server.

use std::time::{Duration, Instant};

use slath_tests::server::{PASSWORD, TestServer};

fn key(name: &str) -> String {
    let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../crates/auth/tests/data/ssh")
        .join(name);
    std::fs::read_to_string(p).unwrap().trim().to_owned()
}

fn lookup(s: &TestServer, fp: &str) -> (bool, String) {
    let sock = s.ipc_socket();
    let (ok, out, _) = s.slath(&["ssh-keys", "--socket", sock.to_str().unwrap(), fp], None);
    (ok, out)
}

#[test]
fn conf_git_02_key_lookup_is_exact() {
    let s = TestServer::start_with(&[]);
    s.create_user("alice", PASSWORD, false);
    s.add_ssh_key("alice", &key("ed25519.pub"));
    let fp = key("ed25519.fp");
    let (ok, line) = lookup(&s, &fp);
    assert!(ok);
    assert!(line.starts_with("restrict,command=\""), "{line}");
    assert!(line.contains(" ssh-serve --socket "), "{line}");
    assert!(
        line.trim_end()
            .ends_with(key("ed25519.pub").split_whitespace().nth(1).unwrap()),
        "{line}"
    );
    // Case changes, prefixes, wildcards, padding, and junk find nothing.
    let flipped: String = fp
        .chars()
        .map(|c| {
            if c.is_ascii_lowercase() {
                c.to_ascii_uppercase()
            } else {
                c.to_ascii_lowercase()
            }
        })
        .collect();
    for probe in [
        flipped.as_str(),
        &fp[..fp.len() - 1],
        &format!("{}%", &fp[..fp.len() - 1]),
        &format!("{fp}="),
        "SHA256:_",
        &"x".repeat(10_000),
    ] {
        let (ok, line) = lookup(&s, probe);
        assert!(ok, "{probe}");
        assert!(line.is_empty(), "{probe}: {line}");
    }
}

#[test]
fn conf_auth_03_deleted_keys_and_suspended_users_stop_working() {
    let s = TestServer::start_with(&[]);
    s.create_user("alice", PASSWORD, false);
    s.add_ssh_key("alice", &key("ed25519.pub"));
    let fp = key("ed25519.fp");
    assert!(!lookup(&s, &fp).1.is_empty());
    s.db.execute("UPDATE users SET state = 'suspended'");
    assert!(lookup(&s, &fp).1.is_empty());
    s.db.execute("UPDATE users SET state = 'active'");
    s.db.execute("DELETE FROM ssh_keys");
    assert!(lookup(&s, &fp).1.is_empty());
}

#[test]
fn conf_git_02_key_lookup_is_fast_with_100k_keys() {
    let s = TestServer::start_with(&[]);
    s.create_user("alice", PASSWORD, false);
    s.add_ssh_key("alice", &key("ed25519.pub"));
    // 100,000 more keys with distinct fingerprints.
    s.db.execute(
        "INSERT INTO ssh_keys (id, user_id, title, key_type, public_key, fingerprint_sha256)
         SELECT gen_random_uuid(), (SELECT id FROM users LIMIT 1), 'k' || i, 'ssh-ed25519', 'AAAA',
                'SHA256:' || substr(translate(encode(sha256(i::text::bytea), 'base64'), '=', 'A'), 1, 43)
         FROM generate_series(1, 100000) AS i;
         ANALYZE ssh_keys;",
    );
    let fp = key("ed25519.fp");
    let mut times = Vec::new();
    for _ in 0..50 {
        let start = Instant::now();
        let (ok, line) = lookup(&s, &fp);
        times.push(start.elapsed());
        assert!(ok && !line.is_empty());
    }
    times.sort();
    let p99 = times[times.len() * 99 / 100];
    // The whole helper process, including startup and IPC, stays well under 100 ms.
    assert!(p99 < Duration::from_millis(100), "p99 {p99:?}");
}
