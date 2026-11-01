// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! The SSH key lookup (`slath ssh-keys`) against the real server, and the
//! separation between the SSH login and the server.

use std::io::{BufRead, BufReader, Write};
use std::os::fd::AsFd;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::os::unix::net::UnixStream;
use std::time::{Duration, Instant};

use slath_git::ipc::{self, Request, Response};
use slath_tests::server::{DEMO, PASSWORD, TestServer, socket_gid};
use slath_tests::sshd::{self, Sshd};

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
    // The whole helper process, including startup and IPC, stays under
    // 100 ms. The 90th percentile tolerates scheduler hiccups when the
    // whole suite runs in parallel; the plan check below makes the
    // independence of the table size deterministic.
    let p90 = times[times.len() * 9 / 10];
    assert!(
        p90 < Duration::from_millis(100),
        "p90 {p90:?}, all {times:?}"
    );
    let top = s.db.query_one(&format!(
        "EXPLAIN SELECT id FROM ssh_keys WHERE fingerprint_sha256 = '{fp}'"
    ));
    assert!(top.contains("Index"), "the lookup must use an index: {top}");
}

/// Sends an SSH session request for alice's demo repository and returns the
/// open connection with the server's first answer.
fn open_session(s: &TestServer, key_id: &str) -> (UnixStream, Response) {
    let stream = UnixStream::connect(s.ipc_socket()).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(20)))
        .unwrap();
    let req = Request::SshSession {
        key_id: key_id.to_owned(),
        service: "git-upload-pack".to_owned(),
        namespace: DEMO.namespace.to_owned(),
        repo: DEMO.name.to_owned(),
        git_protocol: None,
    };
    let mut line = serde_json::to_vec(&req).unwrap();
    line.push(b'\n');
    (&stream).write_all(&line).unwrap();
    let resp = read_answer(&stream);
    (stream, resp)
}

fn read_answer(stream: &UnixStream) -> Response {
    let mut line = Vec::new();
    BufReader::new(stream).read_until(b'\n', &mut line).unwrap();
    serde_json::from_slice(&line).unwrap_or_else(|e| panic!("{e}: {line:?}"))
}

#[test]
fn conf_sec_01_ssh_sessions_need_exactly_the_channel_descriptors() {
    let s = TestServer::start_with(&[DEMO]);
    s.add_ssh_key("alice", &key("ed25519.pub"));
    let key_id = s.db.query_one("SELECT id::text FROM ssh_keys");

    // Unknown keys learn nothing, and get no go-ahead.
    let (_, r) = open_session(&s, "0192f1a2-3b4c-7d5e-8f60-718293a4b5c6");
    assert!(!r.allow);
    assert_eq!(r.messages, ["slath: repository not found"]);

    // No descriptors, or too few: denied, and no Git runs.
    let (stream, r) = open_session(&s, &key_id);
    assert!(r.allow, "{r:?}");
    (&stream).write_all(b"\0").unwrap();
    let r = read_answer(&stream);
    assert!(!r.allow && r.exit_code.is_none(), "{r:?}");

    let (stream, r) = open_session(&s, &key_id);
    assert!(r.allow, "{r:?}");
    let (a, _b) = UnixStream::pair().unwrap();
    ipc::send_fds(&stream, &[a.as_fd(), a.as_fd()]).unwrap();
    let r = read_answer(&stream);
    assert!(!r.allow && r.exit_code.is_none(), "{r:?}");

    // Anything sent along with the request line is refused.
    let stream = UnixStream::connect(s.ipc_socket()).unwrap();
    (&stream)
        .write_all(b"{\"type\":\"lookup_key\",\"fingerprint\":\"SHA256:x\"}\n\0")
        .unwrap();
    let r = read_answer(&stream);
    assert_eq!(r.messages, ["slath: malformed request"]);

    // With the three descriptors, Git runs in the server on them (here: the
    // ref advertisement of the empty repository); closing stdin ends it.
    let (stream, r) = open_session(&s, &key_id);
    assert!(r.allow, "{r:?}");
    let (stdin_ours, stdin_theirs) = UnixStream::pair().unwrap();
    let (stdout_ours, stdout_theirs) = UnixStream::pair().unwrap();
    let (_stderr_ours, stderr_theirs) = UnixStream::pair().unwrap();
    ipc::send_fds(
        &stream,
        &[
            stdin_theirs.as_fd(),
            stdout_theirs.as_fd(),
            stderr_theirs.as_fd(),
        ],
    )
    .unwrap();
    drop((stdin_theirs, stdout_theirs, stderr_theirs));
    drop(stdin_ours);
    let mut advert = Vec::new();
    BufReader::new(&stdout_ours)
        .read_until(b'\n', &mut advert)
        .unwrap();
    assert!(
        advert.windows(10).any(|w| w == b"agent=git/"),
        "no ref advertisement: {}",
        String::from_utf8_lossy(&advert)
    );
    let r = read_answer(&stream);
    assert!(r.exit_code.is_some(), "{r:?}");
}

#[test]
fn conf_sec_01_ssh_login_reaches_only_the_ipc_socket() {
    if sshd::skipped() {
        eprintln!("SLATH_SKIP_SSH_TESTS=1: skipping");
        return;
    }
    let s = TestServer::start_with(&[]);
    // The server shares its run directory with the socket group only, and
    // nothing at all with anyone else.
    let gid = socket_gid();
    let sock = s.ipc_socket();
    let run_dir = sock.parent().unwrap();
    for (path, mode) in [(&*sock, 0o660), (run_dir, 0o710)] {
        let m = std::fs::metadata(path).unwrap();
        assert_eq!(m.gid(), gid, "{}", path.display());
        assert_eq!(m.permissions().mode() & 0o7777, mode, "{}", path.display());
    }
    let (ok, out, _) = s.slath(&["config", "check", "--output", "json"], None);
    let report: serde_json::Value = serde_json::from_str(&out).unwrap();
    let socket_check = report["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == "socket")
        .unwrap();
    assert_eq!(socket_check["ok"], true, "{report} (all ok: {ok})");

    let d = Sshd::start(&s);
    // The login is a user of its own.
    let (ok, uid, err) = d.exec_as("git", &["id", "-u"]);
    assert!(ok, "{err}");
    assert_eq!(uid.trim(), sshd::login_uid().to_string());
    assert_ne!(uid.trim(), sshd::server_uid().to_string());

    let lookup = [
        "slath",
        "ssh-keys",
        "--socket",
        "/run/slath/slath.sock",
        "SHA256:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
    ];
    // Through the socket group it can ask the server (an unknown key: no line)...
    let (ok, out, err) = d.exec_as("git", &lookup);
    assert!(ok, "{err}");
    assert!(out.is_empty(), "{out}");
    // ...which a user outside the group can't...
    let (ok, _, err) = d.exec_as("nobody", &lookup);
    assert!(!ok);
    assert!(err.contains("ermission denied"), "{err}");
    // ...and it can't list the run directory.
    let (ok, _, _) = d.exec_as("git", &["ls", "/run/slath"]);
    assert!(!ok, "the run directory must not be listable");
}
