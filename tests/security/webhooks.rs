// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Webhooks (API-02): deliveries are signed with a timestamp, retried with
//! a stable delivery ID, bounded, checked against the address policy at
//! every attempt, and only carry what the webhook's principal can read.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use slath_tests::server::{DEMO, GitClient, PASSWORD, TestServer};

const LOCAL: &str = "[webhooks]\nallow_http = true\nallow_networks = [\"127.0.0.0/8\"]\n";

/// A request the receiver got.
#[derive(Debug, Clone)]
struct Got {
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Got {
    fn header(&self, name: &str) -> &str {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map_or("", |(_, v)| v.as_str())
    }

    fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap()
    }
}

/// A receiver on 127.0.0.1 that answers each request with the next status
/// of `statuses` (the last one repeats) and records what it got.
fn receiver(statuses: Vec<u16>) -> (u16, Arc<Mutex<Vec<Got>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let got = Arc::new(Mutex::new(Vec::new()));
    let seen = Arc::clone(&got);
    std::thread::spawn(move || {
        for (i, conn) in listener.incoming().enumerate() {
            let Ok(mut c) = conn else { continue };
            let mut r = BufReader::new(c.try_clone().unwrap());
            let mut headers = Vec::new();
            let mut line = String::new();
            r.read_line(&mut line).unwrap();
            loop {
                line.clear();
                if r.read_line(&mut line).unwrap() == 0 || line == "\r\n" {
                    break;
                }
                if let Some((k, v)) = line.trim_end().split_once(':') {
                    headers.push((k.trim().to_owned(), v.trim().to_owned()));
                }
            }
            let len: usize = headers
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case("content-length"))
                .and_then(|(_, v)| v.parse().ok())
                .unwrap_or(0);
            let mut body = vec![0; len];
            r.read_exact(&mut body).unwrap();
            seen.lock().unwrap().push(Got { headers, body });
            let status = statuses[i.min(statuses.len() - 1)];
            let _ = write!(
                c,
                "HTTP/1.1 {status} X\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok"
            );
        }
    });
    (port, got)
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::new(), |mut o, b| {
        let _ = write!(o, "{b:02x}");
        o
    })
}

/// The signature a receiver computes, independently of Slath's code.
fn expected_signature(secret: &str, timestamp: &str, body: &[u8]) -> String {
    use hmac::{Hmac, KeyInit, Mac};
    let mut mac = Hmac::<sha2::Sha256>::new_from_slice(secret.as_bytes()).unwrap();
    mac.update(timestamp.as_bytes());
    mac.update(b".");
    mac.update(body);
    format!("sha256={}", hex(&mac.finalize().into_bytes()))
}

fn api(
    s: &TestServer,
    token: &str,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> slath_tests::http::Resp {
    s.client().api(method, path, Some(token), body.as_ref())
}

fn wait_for(what: &str, s: &TestServer, mut ok: impl FnMut() -> bool) {
    let start = Instant::now();
    while !ok() {
        assert!(
            start.elapsed() < Duration::from_secs(30),
            "{what}\n{}",
            s.log()
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn deliveries(s: &TestServer, token: &str, hook: &str) -> Vec<Value> {
    api(
        s,
        token,
        "GET",
        &format!("/api/v1/repos/alice/demo/webhooks/{hook}/deliveries"),
        None,
    )
    .json()
    .as_array()
    .unwrap()
    .clone()
}

#[test]
fn conf_api_02_deliveries_are_signed_and_carry_the_event() {
    let s = TestServer::start_full(&[DEMO], LOCAL);
    let mut alice = s.login("alice", PASSWORD);
    let token = slath_tests::http::create_token(&mut alice, "hooks", &["repo:write"]);
    let (port, got) = receiver(vec![200]);
    let secret = "a shared secret of some length";
    let r = api(
        &s,
        &token,
        "POST",
        "/api/v1/repos/alice/demo/webhooks",
        Some(
            json!({"url": format!("http://127.0.0.1:{port}/hook?k=1"), "events": ["push"], "secret": secret}),
        ),
    );
    assert_eq!(r.status, 201, "{}", r.body);
    let hook = r.json();
    assert!(
        hook.get("secret").is_none(),
        "a given secret isn't echoed: {hook}"
    );
    let id = hook["id"].as_str().unwrap().to_owned();

    // A push arrives signed over timestamp and body.
    let git = GitClient::new();
    let d = tempfile::tempdir().unwrap();
    let w = d.path();
    git.ok(w, &["init", "-q", "-b", "main"]);
    std::fs::write(w.join("README"), "hello\n").unwrap();
    git.ok(w, &["add", "."]);
    git.ok(w, &["commit", "-q", "-m", "First commit"]);
    git.ok(w, &["push", "-q", &s.url(&DEMO), "main"]);
    wait_for("no push delivery", &s, || !got.lock().unwrap().is_empty());
    let g = got.lock().unwrap()[0].clone();
    assert_eq!(g.header("X-Slath-Event"), "push");
    let ts = g.header("X-Slath-Timestamp").to_owned();
    let age = time::OffsetDateTime::now_utc().unix_timestamp() - ts.parse::<i64>().unwrap();
    assert!((0..60).contains(&age), "{ts}");
    assert_eq!(
        g.header("X-Slath-Signature-256"),
        expected_signature(secret, &ts, &g.body)
    );
    assert_ne!(
        g.header("X-Slath-Signature-256"),
        expected_signature("another secret entirely", &ts, &g.body)
    );
    let p = g.json();
    assert_eq!(p["event"], "push");
    assert_eq!(p["action"], "created");
    assert_eq!(p["repository"]["full_name"], "alice/demo");
    assert_eq!(p["sender"]["username"], "alice");
    assert_eq!(p["push"]["ref"], "refs/heads/main");
    assert_eq!(p["push"]["commits"][0]["message"], "First commit");
    // The delivery log has it.
    wait_for("not delivered", &s, || {
        deliveries(&s, &token, &id)
            .first()
            .is_some_and(|d| d["state"] == "delivered")
    });
    let did = deliveries(&s, &token, &id)[0]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(g.header("X-Slath-Delivery"), did);
    let detail = api(
        &s,
        &token,
        "GET",
        &format!("/api/v1/repos/alice/demo/webhooks/{id}/deliveries/{did}"),
        None,
    )
    .json();
    assert_eq!(detail["attempts"][0]["status"], 200, "{detail}");
    assert_eq!(detail["attempts"][0]["response"], "ok", "{detail}");
    // Redelivery sends the same payload with a new ID.
    let r = api(
        &s,
        &token,
        "POST",
        &format!("/api/v1/repos/alice/demo/webhooks/{id}/deliveries/{did}/redeliver"),
        None,
    );
    assert_eq!(r.status, 202, "{}", r.body);
    wait_for("no redelivery", &s, || got.lock().unwrap().len() == 2);
    let again = got.lock().unwrap()[1].clone();
    assert_ne!(again.header("X-Slath-Delivery"), did);
    assert_eq!(again.json(), p);
    // A generated secret is shown once.
    let r = api(
        &s,
        &token,
        "POST",
        "/api/v1/repos/alice/demo/webhooks",
        Some(json!({"url": format!("http://127.0.0.1:{port}/b"), "events": ["issue"]})),
    );
    assert_eq!(r.status, 201, "{}", r.body);
    let generated = r.json()["secret"].as_str().unwrap().to_owned();
    assert!(generated.len() >= 16);
    let listed = api(&s, &token, "GET", "/api/v1/repos/alice/demo/webhooks", None).body;
    assert!(
        !listed.contains(&generated) && !listed.contains(secret),
        "{listed}"
    );
    assert_eq!(
        s.db.query_one("SELECT count(*)::text FROM audit_events WHERE action = 'webhook.create'"),
        "2"
    );
}

#[test]
fn conf_api_02_retries_keep_the_delivery_id_and_failures_switch_it_off() {
    let s = TestServer::start_full(&[DEMO], LOCAL);
    let mut alice = s.login("alice", PASSWORD);
    let token = slath_tests::http::create_token(&mut alice, "hooks", &["repo:write"]);
    // A redirect is a failure, not followed; then an error; then success.
    let (port, got) = receiver(vec![302, 500, 204]);
    let r = api(
        &s,
        &token,
        "POST",
        "/api/v1/repos/alice/demo/webhooks",
        Some(
            json!({"url": format!("http://127.0.0.1:{port}/"), "events": ["push"], "secret": "0123456789abcdef"}),
        ),
    );
    let id = r.json()["id"].as_str().unwrap().to_owned();
    let r = api(
        &s,
        &token,
        "POST",
        &format!("/api/v1/repos/alice/demo/webhooks/{id}/ping"),
        None,
    );
    assert_eq!(r.status, 202, "{}", r.body);
    let did = r.json()["delivery_id"].as_str().unwrap().to_owned();
    let due_now = || {
        s.db.execute(&format!(
            "UPDATE jobs SET run_at = now() WHERE kind = 'webhook.deliver' AND payload->>'delivery_id' = '{did}' AND state = 'ready'"
        ));
    };
    for n in 1..=3 {
        wait_for("no attempt", &s, || got.lock().unwrap().len() == n);
        if n < 3 {
            // The next attempt waits (1, then 2 minutes); don't.
            wait_for("no retry queued", &s, || {
                s.db.query_one(&format!(
                    "SELECT count(*)::text FROM jobs WHERE kind = 'webhook.deliver' AND payload->>'delivery_id' = '{did}' AND state = 'ready' AND run_at > now() + interval '30 seconds'"
                )) == "1"
            });
            due_now();
        }
    }
    let ids: Vec<String> = got
        .lock()
        .unwrap()
        .iter()
        .map(|g| g.header("X-Slath-Delivery").to_owned())
        .collect();
    assert_eq!(ids, vec![did.clone(); 3]);
    wait_for("not delivered", &s, || {
        api(
            &s,
            &token,
            "GET",
            &format!("/api/v1/repos/alice/demo/webhooks/{id}/deliveries/{did}"),
            None,
        )
        .json()["state"]
            == "delivered"
    });
    let detail = api(
        &s,
        &token,
        "GET",
        &format!("/api/v1/repos/alice/demo/webhooks/{id}/deliveries/{did}"),
        None,
    )
    .json();
    let errors: Vec<&Value> = detail["attempts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| &a["error"])
        .collect();
    assert_eq!(errors[0], "redirects aren't followed", "{detail}");
    assert_eq!(errors[1], "the receiver answered 500", "{detail}");
    assert!(errors[2].is_null());
    let hook = api(
        &s,
        &token,
        "GET",
        &format!("/api/v1/repos/alice/demo/webhooks/{id}"),
        None,
    )
    .json();
    assert_eq!(hook["consecutive_failures"], 0, "success resets the count");

    // After 50 failed attempts in a row, the webhook is switched off.
    let (bad, _) = receiver(vec![500]);
    s.db.execute(&format!(
        "UPDATE webhooks SET url = 'http://127.0.0.1:{bad}/', consecutive_failures = 49 WHERE id = '{id}'"
    ));
    let r = api(
        &s,
        &token,
        "POST",
        &format!("/api/v1/repos/alice/demo/webhooks/{id}/ping"),
        None,
    );
    assert_eq!(r.status, 202);
    wait_for("not switched off", &s, || {
        api(
            &s,
            &token,
            "GET",
            &format!("/api/v1/repos/alice/demo/webhooks/{id}"),
            None,
        )
        .json()["active"]
            == false
    });
    let hook = api(
        &s,
        &token,
        "GET",
        &format!("/api/v1/repos/alice/demo/webhooks/{id}"),
        None,
    )
    .json();
    assert_eq!(
        hook["disabled_reason"], "50 failed deliveries in a row",
        "{hook}"
    );
    assert_eq!(
        s.db.query_one("SELECT count(*)::text FROM audit_events WHERE action = 'webhook.disabled'"),
        "1"
    );
}

#[test]
fn conf_api_02_internal_destinations_are_refused_at_every_attempt() {
    // Without an allowed network, loopback and private addresses are
    // refused when the webhook is saved.
    let strict = TestServer::start_full(&[DEMO], "[webhooks]\nallow_http = true\n");
    let mut alice = strict.login("alice", PASSWORD);
    let token = slath_tests::http::create_token(&mut alice, "hooks", &["repo:write"]);
    for url in [
        "http://127.0.0.1:9/",
        "http://localhost:9/",
        "http://[::1]:9/",
        "http://10.1.2.3/",
        "http://169.254.169.254/latest/meta-data",
        "http://[::ffff:127.0.0.1]/",
        "ftp://example.org/",
    ] {
        let r = api(
            &strict,
            &token,
            "POST",
            "/api/v1/repos/alice/demo/webhooks",
            Some(json!({"url": url, "events": ["push"]})),
        );
        assert_eq!(r.status, 422, "{url}: {}", r.body);
    }
    drop(strict);

    // A destination that becomes internal after it was saved (as with DNS
    // rebinding) is checked again at the attempt, and nothing connects.
    let s = TestServer::start_full(&[DEMO], LOCAL);
    let mut alice = s.login("alice", PASSWORD);
    let token = slath_tests::http::create_token(&mut alice, "hooks", &["repo:write"]);
    let (port, got) = receiver(vec![200]);
    let r = api(
        &s,
        &token,
        "POST",
        "/api/v1/repos/alice/demo/webhooks",
        Some(json!({"url": format!("http://127.0.0.1:{port}/"), "events": ["push"]})),
    );
    let id = r.json()["id"].as_str().unwrap().to_owned();
    s.db.execute(&format!(
        "UPDATE webhooks SET url = 'http://10.0.0.1:{port}/' WHERE id = '{id}'"
    ));
    let r = api(
        &s,
        &token,
        "POST",
        &format!("/api/v1/repos/alice/demo/webhooks/{id}/ping"),
        None,
    );
    let did = r.json()["delivery_id"].as_str().unwrap().to_owned();
    wait_for("no attempt", &s, || {
        api(
            &s,
            &token,
            "GET",
            &format!("/api/v1/repos/alice/demo/webhooks/{id}/deliveries/{did}"),
            None,
        )
        .json()["attempts"]
            .as_array()
            .is_some_and(|a| !a.is_empty())
    });
    let detail = api(
        &s,
        &token,
        "GET",
        &format!("/api/v1/repos/alice/demo/webhooks/{id}/deliveries/{did}"),
        None,
    )
    .json();
    assert!(
        detail["attempts"][0]["error"]
            .as_str()
            .unwrap()
            .contains("internal or reserved network"),
        "{detail}"
    );
    assert!(got.lock().unwrap().is_empty());
}

#[test]
fn conf_api_02_events_reach_only_principals_who_can_read_them() {
    let s = TestServer::start_full(&[DEMO], LOCAL);
    s.create_user("bob", PASSWORD, false);
    let mut alice = s.login("alice", PASSWORD);
    let alice_token = slath_tests::http::create_token(&mut alice, "a", &["repo:write"]);
    let r = api(
        &s,
        &alice_token,
        "PUT",
        "/api/v1/repos/alice/demo/collaborators/bob",
        Some(json!({"role": "admin"})),
    );
    assert!(r.status < 300, "{}", r.body);
    let mut bob = s.login("bob", PASSWORD);
    let bob_token = slath_tests::http::create_token(&mut bob, "b", &["repo:write", "issues:write"]);
    let (port, got) = receiver(vec![200]);
    let r = api(
        &s,
        &bob_token,
        "POST",
        "/api/v1/repos/alice/demo/webhooks",
        Some(json!({"url": format!("http://127.0.0.1:{port}/"), "events": ["issue"]})),
    );
    assert_eq!(r.status, 201, "{}", r.body);
    let id = r.json()["id"].as_str().unwrap().to_owned();
    // Bob's webhook hears about a new issue while he can read the repository.
    let mut issues = s.login("alice", PASSWORD);
    let issues_token = slath_tests::http::create_token(&mut issues, "i", &["issues:write"]);
    let r = api(
        &s,
        &issues_token,
        "POST",
        "/api/v1/repos/alice/demo/issues",
        Some(json!({"title": "First"})),
    );
    assert_eq!(r.status, 201, "{}", r.body);
    wait_for("no issue delivery", &s, || got.lock().unwrap().len() == 1);
    let p = got.lock().unwrap()[0].json();
    assert_eq!(
        (p["event"].as_str(), p["action"].as_str()),
        (Some("issue"), Some("opened"))
    );
    assert_eq!(p["issue"]["title"], "First");
    // Once Bob can't read it any more, nothing is queued for his webhook.
    let r = api(
        &s,
        &alice_token,
        "DELETE",
        "/api/v1/repos/alice/demo/collaborators/bob",
        None,
    );
    assert!(r.status < 300, "{}", r.body);
    let r = api(
        &s,
        &issues_token,
        "POST",
        "/api/v1/repos/alice/demo/issues",
        Some(json!({"title": "Second"})),
    );
    assert_eq!(r.status, 201, "{}", r.body);
    std::thread::sleep(Duration::from_secs(2));
    assert_eq!(got.lock().unwrap().len(), 1);
    assert_eq!(
        s.db.query_one(&format!(
            "SELECT count(*)::text FROM webhook_deliveries WHERE webhook_id = '{id}'"
        )),
        "1"
    );
    // And Bob can't see or change it there now.
    let r = api(
        &s,
        &bob_token,
        "GET",
        &format!("/api/v1/repos/alice/demo/webhooks/{id}"),
        None,
    );
    assert_eq!(r.status, 404, "{}", r.body);
}

/// The code block after `marker` in the user guide.
fn sample(marker: &str) -> String {
    let doc = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../docs/user/webhooks.md"
    ))
    .unwrap();
    let start = doc
        .find(marker)
        .unwrap_or_else(|| panic!("no {marker} block"));
    let rest = &doc[start + marker.len()..];
    let code = &rest[rest.find('\n').unwrap() + 1..];
    code[..code.find("\n```").unwrap()].to_owned()
}

/// Runs a documented verification sample on a request: genuine, tampered.
fn check_sample(
    program: &str,
    ext: &str,
    code: &str,
    secret: &str,
    g: &Got,
) -> Option<(String, String)> {
    let dir = tempfile::tempdir().unwrap();
    let body = dir.path().join("body");
    std::fs::write(&body, &g.body).unwrap();
    let tampered = dir.path().join("tampered");
    let mut t = g.body.clone();
    t.push(b' ');
    std::fs::write(&tampered, &t).unwrap();
    let script = dir.path().join(format!("verify.{ext}"));
    let harness = if ext == "py" {
        "\nimport sys\nprint(verify(sys.argv[1].encode(), sys.argv[2], sys.argv[3], open(sys.argv[4], 'rb').read()))\n"
    } else {
        "\nconst fs = require('node:fs');\nconst a = process.argv.slice(2);\nconsole.log(verify(a[0], a[1], a[2], fs.readFileSync(a[3])) ? 'True' : 'False');\n"
    };
    std::fs::write(&script, format!("{code}{harness}")).unwrap();
    let run = |file: &std::path::Path| {
        let mut cmd = std::process::Command::new(program);
        if ext == "py" {
            cmd.arg("-I");
        }
        let out = cmd
            .arg(&script)
            .args([
                secret,
                g.header("X-Slath-Timestamp"),
                g.header("X-Slath-Signature-256"),
            ])
            .arg(file)
            .output()
            .ok()?;
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        Some(String::from_utf8_lossy(&out.stdout).trim().to_owned())
    };
    Some((run(&body)?, run(&tampered)?))
}

#[test]
fn conf_api_02_the_documented_signature_checks_pass() {
    let s = TestServer::start_full(&[DEMO], LOCAL);
    let mut alice = s.login("alice", PASSWORD);
    let token = slath_tests::http::create_token(&mut alice, "hooks", &["repo:write"]);
    let (port, got) = receiver(vec![200]);
    let secret = "the receiver's own secret";
    let r = api(
        &s,
        &token,
        "POST",
        "/api/v1/repos/alice/demo/webhooks",
        Some(
            json!({"url": format!("http://127.0.0.1:{port}/"), "events": ["push"], "secret": secret}),
        ),
    );
    let id = r.json()["id"].as_str().unwrap().to_owned();
    api(
        &s,
        &token,
        "POST",
        &format!("/api/v1/repos/alice/demo/webhooks/{id}/ping"),
        None,
    );
    wait_for("no ping", &s, || !got.lock().unwrap().is_empty());
    let g = got.lock().unwrap()[0].clone();
    assert_eq!(g.json()["event"], "ping");
    let python = check_sample("python3", "py", &sample("```python"), secret, &g)
        .expect("python3 is needed to check the documented sample");
    assert_eq!(python, ("True".into(), "False".into()));
    // Node.js is optional on test machines.
    if let Some(js) = check_sample("node", "js", &sample("```js"), secret, &g) {
        assert_eq!(js, ("True".into(), "False".into()));
    }
}

/// The settings pages: adding without a secret shows the generated one
/// once, the delivery log and a delivery's page work, and only
/// administrators (owners for organizations) see any of it.
#[test]
fn webhook_pages_for_administrators_and_owners_only() {
    use slath_tests::http::submit;
    let s = TestServer::start_full(&[DEMO], LOCAL);
    s.create_user("bob", PASSWORD, false);
    let mut alice = s.login("alice", PASSWORD);
    let token = slath_tests::http::create_token(&mut alice, "t", &["repo:write", "user:write"]);
    let r = api(
        &s,
        &token,
        "PUT",
        "/api/v1/repos/alice/demo/collaborators/bob",
        Some(json!({"role": "write"})),
    );
    assert!(r.status < 300, "{}", r.body);
    let (port, got) = receiver(vec![200]);
    let base = "/alice/demo/settings/webhooks";
    let url = format!("http://127.0.0.1:{port}/ui");
    let r = submit(
        &mut alice,
        base,
        base,
        &[
            ("url", url.as_str()),
            ("events", "push"),
            ("events", "issue"),
            ("active", "1"),
        ],
    );
    assert_eq!(r.status, 200, "{}", r.body);
    assert!(r.body.contains("it won't be shown again"), "{}", r.body);
    let secret = r
        .body
        .split("id=\"new-secret\"")
        .nth(1)
        .and_then(|t| t.split("value=\"").nth(1))
        .and_then(|t| t.split('"').next())
        .unwrap()
        .to_owned();
    assert!(secret.len() >= 16);
    let list = alice.get(base).body;
    assert!(
        list.contains(&url) && list.contains("push, issue"),
        "{list}"
    );
    assert!(!list.contains(&secret));
    let hook = s.db.query_one("SELECT id::text FROM webhooks");
    let page = format!("{base}/{hook}");
    assert!(!alice.get(&page).body.contains(&secret), "shown once only");
    // A ping shows in the log and has its own page.
    let r = submit(&mut alice, &page, &format!("{page}/ping"), &[]);
    assert_eq!(r.status, 303, "{}", r.body);
    wait_for("no ping", &s, || !got.lock().unwrap().is_empty());
    let g = got.lock().unwrap()[0].clone();
    assert_eq!(
        g.header("X-Slath-Signature-256"),
        expected_signature(&secret, g.header("X-Slath-Timestamp"), &g.body)
    );
    let delivery = g.header("X-Slath-Delivery").to_owned();
    wait_for("not delivered", &s, || {
        alice.get(&page).body.contains("Delivered")
    });
    let d = alice.get(&format!("{page}/deliveries/{delivery}"));
    assert_eq!(d.status, 200);
    assert!(
        d.body.contains("Answered 200.") && d.body.contains("Small changes, reviewed well."),
        "{}",
        d.body
    );
    // Changing the events keeps the secret unless a new one is given.
    let r = submit(
        &mut alice,
        &page,
        &page,
        &[("url", url.as_str()), ("events", "status"), ("active", "1")],
    );
    assert_eq!(r.status, 303, "{}", r.body);
    assert!(alice.get(base).body.contains(">status<"));
    // Writers who aren't administrators, and strangers, see nothing.
    let mut bob = s.login("bob", PASSWORD);
    for path in [
        base.to_owned(),
        page.clone(),
        format!("{page}/deliveries/{delivery}"),
    ] {
        assert_eq!(bob.get(&path).status, 404, "{path}");
    }
    let r = submit(
        &mut bob,
        "/-/settings/profile",
        &format!("{page}/delete"),
        &[],
    );
    assert_eq!(r.status, 404, "{}", r.body);
    // Organizations: owners only.
    let r = api(
        &s,
        &token,
        "POST",
        "/api/v1/orgs",
        Some(json!({"name": "acme"})),
    );
    assert_eq!(r.status, 201, "{}", r.body);
    assert_eq!(alice.get("/acme/-/webhooks").status, 200);
    assert_eq!(bob.get("/acme/-/webhooks").status, 404);
    // Deleting removes it.
    let r = submit(&mut alice, &page, &format!("{page}/delete"), &[]);
    assert_eq!(r.status, 303, "{}", r.body);
    assert!(alice.get(base).body.contains("No webhooks yet."));
}
