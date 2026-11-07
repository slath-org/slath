// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Crash and restart (OPS-04): the server is killed with SIGKILL during a
//! push, issue creation, a running job, and an upload, then started again.
//! Nothing half-done becomes visible, refs are never corrupted, the
//! reconciler repairs the derived ref snapshot, and interrupted jobs run
//! again. (Interrupted upgrades and migrations are covered in upgrade.rs.)

use std::io::Write;
use std::net::TcpStream;
use std::time::{Duration, Instant};

use slath_tests::http::{create_token, submit};
use slath_tests::server::{DEMO, GitClient, PASSWORD, TestServer, slath_bin};

fn wait_for(what: &str, secs: u64, mut f: impl FnMut() -> bool) {
    let start = Instant::now();
    while !f() {
        assert!(
            start.elapsed() < Duration::from_secs(secs),
            "timed out: {what}"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn restart(s: &mut TestServer) {
    s.start_again(&slath_bin()).unwrap();
}

/// `refname oid` lines of a bare repository.
fn git_refs(git_dir: &std::path::Path) -> String {
    GitClient::new().ok(
        git_dir,
        &["for-each-ref", "--format=%(refname) %(objectname)"],
    )
}

/// The same from the database's snapshot.
fn db_refs(s: &TestServer) -> String {
    s.db.query_one(
        "SELECT coalesce(string_agg(convert_from(name, 'UTF8') || ' ' || oid, E'\\n' ORDER BY name), '')
           FROM git_refs WHERE repo_id = (SELECT id FROM repositories WHERE name = 'demo')",
    )
}

#[test]
fn conf_ops_04_kill_during_push() {
    let mut s = TestServer::start();
    let git = GitClient::new();
    let d = tempfile::tempdir().unwrap();
    let w = d.path().join("w");
    std::fs::create_dir_all(&w).unwrap();
    git.ok(&w, &["init", "-q", "-b", "main"]);
    std::fs::write(w.join("a.txt"), "first\n").unwrap();
    git.ok(&w, &["add", "-A"]);
    git.ok(&w, &["commit", "-q", "-m", "first"]);
    git.ok(&w, &["push", "-q", &s.url(&DEMO), "main"]);
    let before = git_refs(&s.repo_path(&DEMO));

    // A push big enough to still be running when the server dies.
    let mut seed: u64 = 0x2545_F491_4F6C_DD1D;
    for i in 0..40 {
        let mut data = Vec::with_capacity(1 << 20);
        for _ in 0..(1 << 17) {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            data.extend_from_slice(&seed.to_le_bytes());
        }
        std::fs::write(w.join(format!("blob{i}.bin")), data).unwrap();
    }
    git.ok(&w, &["add", "-A"]);
    git.ok(&w, &["commit", "-q", "-m", "big"]);
    git.ok(&w, &["branch", "feature"]);
    let url = s.url(&DEMO);
    let wd = w.clone();
    let push = std::thread::spawn(move || {
        GitClient::new()
            .cmd(&wd)
            .args(["push", "-q", &url, "main", "feature"])
            .output()
            .unwrap()
    });
    std::thread::sleep(Duration::from_millis(400));
    s.stop();
    let out = push.join().unwrap();
    restart(&mut s);

    // Git has either the old refs or the complete new ones, and both are
    // consistent.
    let after = git_refs(&s.repo_path(&DEMO));
    let local_main = git.ok(&w, &["rev-parse", "main"]);
    if out.status.success() {
        assert!(after.contains(local_main.trim()), "{after}");
    } else {
        assert!(
            after == before || after.contains(local_main.trim()),
            "partial ref state:\n{before}\n--\n{after}"
        );
    }
    GitClient::new().ok(&s.repo_path(&DEMO), &["fsck", "--no-dangling"]);
    // The reconciler brings the snapshot in line with Git after a restart.
    wait_for("the ref snapshot matches Git", 30, || {
        db_refs(&s).trim() == after.trim()
    });
    // And the repository takes the push again.
    git.ok(&w, &["push", "-q", &s.url(&DEMO), "main", "feature"]);
}

#[test]
fn conf_ops_04_kill_during_issue_creation() {
    let mut s = TestServer::start();
    let mut a = s.login("alice", PASSWORD);
    let token = create_token(&mut a, "t", &["issues:write", "repo:read"]);
    let base = s.base();
    let creator = std::thread::spawn(move || {
        let mut c = slath_tests::http::Client::new(&base);
        let mut ok = Vec::new();
        for i in 0..2000 {
            let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                c.api(
                    "POST",
                    "/api/v1/repos/alice/demo/issues",
                    Some(&token),
                    Some(&serde_json::json!({"title": format!("Issue {i}"), "body": "x"})),
                )
            }));
            match r {
                Ok(r) if r.status == 201 => ok.push(r.json()["number"].as_i64().unwrap()),
                _ => break,
            }
        }
        ok
    });
    std::thread::sleep(Duration::from_millis(700));
    s.stop();
    let acknowledged = creator.join().unwrap();
    assert!(
        !acknowledged.is_empty(),
        "the server died before any issue was created"
    );
    assert!(
        acknowledged.len() < 2000,
        "the server outlived the issue creation"
    );
    restart(&mut s);

    // Every acknowledged issue exists, numbers are unique, and the next
    // number continues after the highest one.
    let numbers: Vec<i64> = s
        .db
        .query_one("SELECT coalesce(string_agg(number::text, ',' ORDER BY number), '') FROM issues")
        .split(',')
        .filter(|n| !n.is_empty())
        .map(|n| n.parse().unwrap())
        .collect();
    for n in &acknowledged {
        assert!(numbers.contains(n), "acknowledged issue #{n} is missing");
    }
    let mut dedup = numbers.clone();
    dedup.dedup();
    assert_eq!(dedup.len(), numbers.len());
    let max = numbers.last().copied().unwrap_or(0);
    let next: i64 =
        s.db.query_one("SELECT next_item_number::text FROM repositories WHERE name = 'demo'")
            .parse()
            .unwrap();
    assert!(next > max, "next number {next} <= {max}");
    let mut a = s.login("alice", PASSWORD);
    let r = submit(
        &mut a,
        "/alice/demo/issues/new",
        "/alice/demo/issues",
        &[("title", "After the crash"), ("body", "")],
    );
    assert_eq!(r.status, 303, "{}", r.body);
    assert_eq!(
        r.header("location").unwrap(),
        format!("/alice/demo/issues/{next}")
    );
}

#[test]
fn conf_ops_04_jobs_resume_after_a_crash() {
    let mut s = TestServer::start();
    s.create_user("bob", PASSWORD, false);
    let mut a = s.login("alice", PASSWORD);
    let page = "/alice/demo/settings/access";
    assert_eq!(
        submit(&mut a, page, page, &[("username", "bob"), ("role", "read")]).status,
        303
    );
    s.stop();
    // A fan-out job that was running when the server died: leased, lease
    // expired.
    s.db.execute(
        "INSERT INTO issues (id, repo_id, number, author_id, title)
         SELECT gen_random_uuid(), r.id, 1, u.id, 'Crash'
           FROM repositories r, users u JOIN namespaces n ON n.id = u.namespace_id
          WHERE r.name = 'demo' AND n.name = 'bob'",
    );
    s.db.execute("UPDATE repositories SET next_item_number = 2 WHERE name = 'demo'");
    s.db.execute(
        "INSERT INTO jobs (queue, kind, payload, state, run_at, attempts, locked_by, locked_until)
         SELECT 'default', 'notify.fanout',
                jsonb_build_object('repo_id', i.repo_id::text, 'issue_id', i.id::text,
                                   'actor_id', i.author_id::text, 'activity', 'opened'),
                'running', now() - interval '1 minute', 1, 'dead-worker', now() - interval '1 second'
           FROM issues i",
    );
    restart(&mut s);
    wait_for("the interrupted job ran again", 30, || {
        s.db.query_one("SELECT state FROM jobs WHERE kind = 'notify.fanout'") == "done"
    });
    // alice watches her repository and got the notification exactly once.
    assert_eq!(
        s.db.query_one(
            "SELECT count(*)::text FROM notifications n JOIN users u ON u.id = n.user_id
              JOIN namespaces s ON s.id = u.namespace_id WHERE s.name = 'alice'"
        ),
        "1"
    );
}

#[test]
fn conf_ops_04_kill_during_upload() {
    let mut s = TestServer::start();
    let mut a = s.login("alice", PASSWORD);
    let token = create_token(&mut a, "t", &["issues:write"]);
    // Send the headers and part of a multipart body, then kill the server.
    let boundary = "crash-boundary";
    let head = format!(
        "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"big.bin\"\r\n\r\n"
    );
    let total = head.len() + 4 * 1024 * 1024;
    let mut conn = TcpStream::connect(s.base().trim_start_matches("http://")).unwrap();
    write!(
        conn,
        "POST /api/v1/repos/alice/demo/attachments HTTP/1.1\r\nHost: x\r\nAuthorization: Bearer {token}\r\nContent-Type: multipart/form-data; boundary={boundary}\r\nContent-Length: {}\r\n\r\n{head}",
        total + 100
    )
    .unwrap();
    for _ in 0..16 {
        conn.write_all(&[0xA5; 64 * 1024]).unwrap();
    }
    conn.flush().unwrap();
    std::thread::sleep(Duration::from_millis(300));
    s.stop();
    drop(conn);
    restart(&mut s);

    assert_eq!(
        s.db.query_one("SELECT count(*)::text FROM attachments"),
        "0"
    );
    assert_eq!(s.db.query_one("SELECT count(*)::text FROM blobs"), "0");
    // Nothing reached the store; only a temporary file (deleted by the
    // collector later) may remain.
    let blobs = s.data_dir().join("blobs");
    if let Ok(dirs) = std::fs::read_dir(&blobs) {
        for d in dirs.flatten() {
            assert_eq!(d.file_name(), "tmp", "unexpected {:?}", d.path());
        }
    }
    // Uploads work again.
    let (ct, body) = slath_tests::http::multipart(&[("file", Some("ok.txt"), b"fine")]);
    let auth = format!("Bearer {token}");
    let r = a.request(
        "POST",
        "/api/v1/repos/alice/demo/attachments",
        &[("authorization", &auth), ("cookie", "")],
        Some((&ct, body)),
    );
    assert_eq!(r.status, 201, "{}", r.body);
}
