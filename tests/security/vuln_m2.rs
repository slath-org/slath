// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Regression tests from published forge advisories for the surfaces added
//! in 0.2: imports, SSRF, attachments and avatars, Markdown, cross-repository
//! IDs, password resets, and OpenID Connect. Each test names the advisory
//! (or advisory family) it guards against.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use serde_json::json;
use slath_tests::http::{Client, Resp, create_repo, create_token, multipart, submit};
use slath_tests::oidc::{MockProvider, state_and_nonce};
use slath_tests::server::{PASSWORD, TestServer};
use slath_tests::smtp::SmtpServer;

/// An HTTP listener that counts connections and answers `answer`.
fn listener(answer: String) -> (u16, Arc<AtomicUsize>) {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    let hits = Arc::new(AtomicUsize::new(0));
    let h = Arc::clone(&hits);
    std::thread::spawn(move || {
        for mut c in l.incoming().flatten() {
            h.fetch_add(1, Ordering::SeqCst);
            let mut buf = [0u8; 4096];
            let _ = c.read(&mut buf);
            let _ = c.write_all(answer.as_bytes());
        }
    });
    (port, hits)
}

fn import(c: &mut Client, token: &str, name: &str, url: &str) -> Resp {
    c.api(
        "POST",
        "/api/v1/repos/import",
        Some(token),
        Some(&json!({"namespace": "alice", "name": name, "url": url})),
    )
}

fn import_status(c: &mut Client, token: &str, name: &str) -> String {
    let start = std::time::Instant::now();
    loop {
        let r = c.api(
            "GET",
            &format!("/api/v1/repos/alice/{name}/import"),
            Some(token),
            None,
        );
        let state = r.json()["state"].as_str().unwrap_or("").to_owned();
        if state == "done" || state == "failed" {
            return state;
        }
        assert!(start.elapsed() < Duration::from_secs(60), "{}", r.body);
        std::thread::sleep(Duration::from_millis(200));
    }
}

fn alice(extra: &str) -> (TestServer, Client, String) {
    let s = TestServer::start_full(&[], extra);
    s.create_user("alice", PASSWORD, false);
    let mut a = s.login("alice", PASSWORD);
    let t = create_token(&mut a, "t", &["repo:write", "issues:write"]);
    (s, a, t)
}

/// Gitea/Gogs migration argument injection (CVE-2022-0415 family, C1):
/// option-like and non-HTTPS import URLs are refused before Git runs.
#[test]
fn conf_sec_01_import_url_option_injection() {
    let (s, mut a, t) = alice("");
    for (i, url) in [
        "--upload-pack=touch /tmp/slath-pwned",
        "-uhttps://x",
        "ext::sh -c touch% /tmp/slath-pwned",
        "fd::17",
        "file:///etc",
        "/srv/slath/repos/aa/0192f1a2-3b4c-7d5e-8f60-718293a4b5c6.git",
        "https://h/x\r",
        "https://h/x\n[core]\nsshCommand=touch /tmp/slath-pwned",
    ]
    .iter()
    .enumerate()
    {
        let r = import(&mut a, &t, &format!("x{i}"), url);
        assert_eq!(r.status, 422, "{url:?}: {}", r.body);
    }
    assert_eq!(
        s.db.query_one("SELECT count(*)::text FROM repo_imports"),
        "0"
    );
    assert!(!std::path::Path::new("/tmp/slath-pwned").exists());
}

/// Git bundle URIs and redirects as an SSRF path during migration (Gitea
/// GHSA-x77v family, C2/C16): the source advertises a bundle URI and the
/// import never contacts it; redirects aren't followed.
#[test]
fn conf_git_06_import_bundle_uri_and_redirect_ignored() {
    let (bundle_port, bundle_hits) =
        listener("HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n".into());
    let src = TestServer::start();
    let mut sa = src.login("alice", PASSWORD);
    let st = create_token(&mut sa, "s", &["repo:read"]);
    let git = slath_tests::server::GitClient::new();
    let w = tempfile::tempdir().unwrap();
    git.ok(w.path(), &["init", "-q", "-b", "main"]);
    std::fs::write(w.path().join("f"), "x\n").unwrap();
    git.ok(w.path(), &["add", "-A"]);
    git.ok(w.path(), &["commit", "-q", "-m", "x"]);
    git.ok(
        w.path(),
        &["push", "-q", &src.url(&slath_tests::server::DEMO), "main"],
    );
    let repo = src.repo_path(&slath_tests::server::DEMO);
    for (k, v) in [
        ("uploadpack.advertiseBundleURIs", "true".to_owned()),
        ("bundle.version", "1".to_owned()),
        ("bundle.mode", "all".to_owned()),
        (
            "bundle.evil.uri",
            format!("http://127.0.0.1:{bundle_port}/evil.bundle"),
        ),
    ] {
        git.ok(&repo, &["config", "--file", "config", k, &v]);
    }
    let (s, mut a, t) = alice("[import]\nallow_http = true\nallow_networks = [\"127.0.0.0/8\"]\n");
    let url = format!("{}/alice/demo.git", src.base());
    let r = a.api(
        "POST",
        "/api/v1/repos/import",
        Some(&t),
        Some(&json!({"namespace": "alice", "name": "b", "url": url, "username": "alice", "password": st})),
    );
    assert_eq!(r.status, 202, "{}", r.body);
    assert_eq!(import_status(&mut a, &t, "b"), "done", "{}", s.log());
    assert_eq!(
        bundle_hits.load(Ordering::SeqCst),
        0,
        "the bundle URI was fetched"
    );

    let (internal, internal_hits) = listener(String::new());
    let (redir, _) = listener(format!(
        "HTTP/1.1 301 Moved Permanently\r\nLocation: http://127.0.0.1:{internal}/x.git/info/refs?service=git-upload-pack\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
    ));
    let r = import(&mut a, &t, "r", &format!("http://127.0.0.1:{redir}/x.git"));
    assert_eq!(r.status, 202, "{}", r.body);
    assert_eq!(import_status(&mut a, &t, "r"), "failed");
    assert_eq!(
        internal_hits.load(Ordering::SeqCst),
        0,
        "the redirect was followed"
    );
}

/// Attachment path traversal through the file name (Gitea attachment and
/// Forgejo CVE-2023 path issues, C3): the name segment never reaches the
/// file system and `Content-Disposition` stays well-formed.
#[test]
fn conf_rbac_02_attachment_filename_path_ignored() {
    let (_s, mut a, t) = alice("");
    create_repo(&mut a, "alice", "pub", "public");
    let (ct, body) = multipart(&[("file", Some("a\";b.txt"), b"attachment bytes")]);
    let auth = format!("Bearer {t}");
    let up = a.request(
        "POST",
        "/api/v1/repos/alice/pub/attachments",
        &[("authorization", &auth), ("cookie", "")],
        Some((&ct, body)),
    );
    assert_eq!(up.status, 201, "{}", up.body);
    let id = up.json()["id"].as_str().unwrap().to_owned();
    for name in ["..%2f..%2fetc%2fpasswd", "other.png", "%2e%2e", "x"] {
        let r = a.get(&format!("/alice/pub/attachments/{id}/{name}"));
        assert_eq!(r.status, 200, "{name}");
        assert_eq!(r.bytes, b"attachment bytes", "{name}");
        let d = r.header("content-disposition").unwrap();
        assert!(!d.contains('\r') && !d.contains('\n'));
        assert_eq!(d.matches('"').count(), 2, "{d}");
    }
}

/// Attachments readable through another repository (Forgejo CVE-2023-49946
/// and Gitea attachment IDOR advisories, C8).
#[test]
fn conf_rbac_02_attachment_bound_to_repo() {
    let (s, mut a, t) = alice("");
    create_repo(&mut a, "alice", "priv", "private");
    let (ct, body) = multipart(&[("file", Some("s.txt"), b"private bytes")]);
    let auth = format!("Bearer {t}");
    let up = a.request(
        "POST",
        "/api/v1/repos/alice/priv/attachments",
        &[("authorization", &auth), ("cookie", "")],
        Some((&ct, body)),
    );
    let id = up.json()["id"].as_str().unwrap().to_owned();
    s.create_user("mallory", PASSWORD, false);
    let mut m = s.login("mallory", PASSWORD);
    create_repo(&mut m, "mallory", "pub", "public");
    let r = submit(
        &mut m,
        "/mallory/pub/issues/new",
        "/mallory/pub/issues",
        &[
            ("title", "x"),
            ("body", &format!("![x](/alice/priv/attachments/{id}/s.txt)")),
        ],
    );
    assert_eq!(r.status, 303);
    let mut anon = s.client();
    assert!(
        !anon
            .get("/mallory/pub/issues/1")
            .body
            .contains("private bytes")
    );
    assert_eq!(
        anon.get(&format!("/mallory/pub/attachments/{id}/s.txt"))
            .status,
        404
    );
    assert_eq!(
        m.get(&format!("/mallory/pub/attachments/{id}/s.txt"))
            .status,
        404
    );
    assert_eq!(
        anon.get(&format!("/alice/priv/attachments/{id}/s.txt"))
            .status,
        404
    );
    assert_eq!(
        m.get(&format!("/alice/priv/attachments/{id}/s.txt")).status,
        404
    );
}

/// Cross-repository IDs for labels, milestones, and comments (Forgejo
/// CVE-2023-49946, Gitea label/comment IDOR fixes, C8).
#[test]
fn conf_rbac_02_cross_repo_ids_issue_objects() {
    let (s, mut a, t) = alice("");
    create_repo(&mut a, "alice", "priv", "private");
    let _ = a.api(
        "POST",
        "/api/v1/repos/alice/priv/labels",
        Some(&t),
        Some(&json!({"name": "secret", "color": "aa0000"})),
    );
    let ms = a.api(
        "POST",
        "/api/v1/repos/alice/priv/milestones",
        Some(&t),
        Some(&json!({"title": "Hidden"})),
    );
    let ms_id = ms.json()["id"].as_str().unwrap_or("0").to_owned();
    assert_ne!(ms_id, "0", "{}", ms.body);
    a.api(
        "POST",
        "/api/v1/repos/alice/priv/issues",
        Some(&t),
        Some(&json!({"title": "x", "body": ""})),
    );
    let c = a.api(
        "POST",
        "/api/v1/repos/alice/priv/issues/1/comments",
        Some(&t),
        Some(&json!({"body": "secret comment"})),
    );
    let comment = c.json()["id"].as_str().unwrap().to_owned();

    s.create_user("mallory", PASSWORD, false);
    let mut m = s.login("mallory", PASSWORD);
    create_repo(&mut m, "mallory", "pub", "public");
    let mt = create_token(&mut m, "t", &["repo:write", "issues:write"]);
    m.api(
        "POST",
        "/api/v1/repos/mallory/pub/issues",
        Some(&mt),
        Some(&json!({"title": "m", "body": ""})),
    );
    for (method, path, body) in [
        (
            "PATCH",
            format!("/api/v1/repos/mallory/pub/issues/comments/{comment}"),
            json!({"body": "pwned"}),
        ),
        (
            "DELETE",
            format!("/api/v1/repos/mallory/pub/issues/comments/{comment}"),
            json!({}),
        ),
        (
            "PATCH",
            format!("/api/v1/repos/mallory/pub/milestones/{ms_id}"),
            json!({"title": "pwned"}),
        ),
        (
            "DELETE",
            format!("/api/v1/repos/mallory/pub/milestones/{ms_id}"),
            json!({}),
        ),
        (
            "DELETE",
            "/api/v1/repos/mallory/pub/labels/secret".to_owned(),
            json!({}),
        ),
        (
            "PATCH",
            "/api/v1/repos/mallory/pub/issues/1".to_owned(),
            json!({"labels": ["secret"], "milestone": "Hidden"}),
        ),
        (
            "PATCH",
            format!("/api/v1/repos/alice/priv/issues/comments/{comment}"),
            json!({"body": "pwned"}),
        ),
    ] {
        let r = m.api(method, &path, Some(&mt), Some(&body));
        assert!(
            matches!(r.status, 404 | 422),
            "{method} {path}: {} {}",
            r.status,
            r.body
        );
    }
    let body = a
        .api(
            "GET",
            "/api/v1/repos/alice/priv/issues/1/comments",
            Some(&t),
            None,
        )
        .body;
    assert!(body.contains("secret comment") && !body.contains("pwned"));
    assert!(
        a.api("GET", "/api/v1/repos/alice/priv/labels", Some(&t), None)
            .body
            .contains("secret")
    );
    assert!(
        a.api("GET", "/api/v1/repos/alice/priv/milestones", Some(&t), None)
            .body
            .contains("Hidden")
    );
}

/// SVG and polyglot avatars (Gitea avatar XSS fixes, C9): SVG is refused
/// and a PNG carrying HTML comes back as a fresh PNG without it.
#[test]
fn conf_sec_04_avatar_svg_rejected() {
    let (_s, mut a, _) = alice("");
    let mut polyglot = Vec::new();
    image::RgbImage::from_pixel(8, 8, image::Rgb([1, 2, 3]))
        .write_to(
            &mut std::io::Cursor::new(&mut polyglot),
            image::ImageFormat::Png,
        )
        .unwrap();
    polyglot.extend_from_slice(b"<html><script>alert(1)</script></html>");
    let send = |a: &mut Client, name: &str, data: &[u8]| {
        let csrf = slath_tests::http::csrf_token(&a.get("/-/settings/profile").body).unwrap();
        let (ct, body) = multipart(&[
            ("_csrf", None, csrf.as_bytes()),
            ("avatar", Some(name), data),
        ]);
        a.request("POST", "/-/settings/avatar", &[], Some((&ct, body)))
    };
    assert_eq!(
        send(
            &mut a,
            "x.svg",
            b"<svg xmlns='http://www.w3.org/2000/svg' onload='alert(1)'/>"
        )
        .status,
        422
    );
    assert_eq!(send(&mut a, "x.png", &polyglot).status, 303);
    let page = a.get("/alice").body;
    let start = page.find("/-/avatars/").unwrap();
    let src = &page[start..start + "/-/avatars/".len() + 68];
    let png = a.get(src);
    assert_eq!(png.header("content-type"), Some("image/png"));
    assert!(!png.bytes.windows(7).any(|w| w == b"<script"));
}

/// Mermaid/KaTeX-style renderers (Gitea and GitLab diagram XSS advisories):
/// such blocks stay code, and math with `\href{javascript:…}` stays text.
#[test]
fn conf_sec_04_diagram_math_not_rendered_unsafely() {
    let (_s, mut a, t) = alice("");
    create_repo(&mut a, "alice", "pub", "public");
    let body = "```mermaid\ngraph TD; A[\"<img src=x onerror=alert(1)>\"]-->B\nclick A \"javascript:alert(1)\"\n```\n\n$$\\href{javascript:alert(1)}{x}$$\n";
    let r = a.api(
        "POST",
        "/api/v1/repos/alice/pub/issues",
        Some(&t),
        Some(&json!({"title": "d", "body": body})),
    );
    let html = r.json()["body_html"].as_str().unwrap().to_owned();
    assert!(html.contains("<code"), "{html}");
    assert!(
        !html.contains("<svg") && !html.contains("href=\"javascript:") && !html.contains("<img"),
        "{html}"
    );
}

/// SSRF address forms (Gitea GHSA-x77v, GitLab import SSRF advisories, C16):
/// every internal or reserved form is refused for imports.
#[test]
fn conf_sec_04_ssrf_address_matrix() {
    let (s, mut a, t) = alice("");
    let own = s
        .base()
        .trim_start_matches("http://")
        .split(':')
        .next()
        .unwrap()
        .to_owned();
    for (i, host) in [
        "127.0.0.1",
        "127.1",
        "0x7f000001",
        "2130706433",
        "0.0.0.0",
        "[::]",
        "[::1]",
        "[::ffff:127.0.0.1]",
        "[::ffff:7f00:1]",
        "[64:ff9b::127.0.0.1]",
        "10.0.0.1",
        "172.16.0.1",
        "192.168.0.1",
        "100.64.0.1",
        "169.254.169.254",
        "[fd00::1]",
        "[fe80::1%25eth0]",
        "224.0.0.1",
        own.as_str(),
        "localhost.",
        "x.127.0.0.1.nip.io",
    ]
    .iter()
    .enumerate()
    {
        let r = import(
            &mut a,
            &t,
            &format!("s{i}"),
            &format!("https://{host}/x.git"),
        );
        assert_eq!(r.status, 422, "{host}: {}", r.body);
    }
}

/// Redirects to internal addresses (Gitea GHSA-x77v and GitLab redirect
/// SSRF, C16): an import whose source redirects never reaches the target.
#[test]
fn conf_sec_04_ssrf_redirects_not_followed() {
    let (internal, internal_hits) = listener(String::new());
    let (redir, redir_hits) = listener(format!(
        "HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1:{internal}/\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
    ));
    let (_s, mut a, t) = alice("[import]\nallow_http = true\nallow_networks = [\"127.0.0.0/8\"]\n");
    let r = import(&mut a, &t, "r", &format!("http://127.0.0.1:{redir}/x.git"));
    assert_eq!(r.status, 202, "{}", r.body);
    assert_eq!(import_status(&mut a, &t, "r"), "failed");
    assert!(redir_hits.load(Ordering::SeqCst) >= 1);
    assert_eq!(internal_hits.load(Ordering::SeqCst), 0);
}

/// Allowlists that skip the address check (Gitea G46 `MatchHostOrIP`):
/// allowing one network doesn't open others.
#[test]
fn conf_sec_04_ssrf_allowlist_does_not_skip_ip_check() {
    let (_s, mut a, t) = alice("[import]\nallow_networks = [\"10.20.0.0/16\"]\n");
    for host in ["127.0.0.1", "10.21.0.1", "169.254.169.254", "localhost"] {
        let r = import(&mut a, &t, "x", &format!("https://{host}/x.git"));
        assert_eq!(r.status, 422, "{host}: {}", r.body);
    }
}

/// Password reset to attacker-controlled recipients (GitLab CVE-2023-7028):
/// arrays, lists, and header injection never add a recipient.
#[test]
fn conf_sec_04_password_reset_email_array() {
    let smtp = SmtpServer::start();
    let s = TestServer::start_full(&[], &smtp.config());
    s.create_user("victim", PASSWORD, false);
    s.db.execute("UPDATE users SET email_verified = true");
    let before = smtp.messages().len();
    let mut c = s.client();
    let page = c.get("/-/password/reset");
    let csrf = slath_tests::http::csrf_token(&page.body).unwrap();
    for body in [
        format!(
            "_csrf={csrf}&email%5B%5D=victim%40example.org&email%5B%5D=attacker%40evil.example"
        ),
        format!("_csrf={csrf}&email=victim%40example.org&email=attacker%40evil.example"),
        format!("_csrf={csrf}&email=victim%40example.org%2Cattacker%40evil.example"),
        format!("_csrf={csrf}&email=victim%40example.org%0aCc%3Aattacker%40evil.example"),
    ] {
        let r = c.request(
            "POST",
            "/-/password/reset",
            &[],
            Some(("application/x-www-form-urlencoded", body.into_bytes())),
        );
        assert!(matches!(r.status, 200 | 303 | 400 | 422), "{}", r.status);
    }
    let r = c.request(
        "POST",
        "/-/password/reset",
        &[("x-csrf-token", &csrf)],
        Some((
            "application/json",
            br#"{"email":["victim@example.org","attacker@evil.example"]}"#.to_vec(),
        )),
    );
    assert!(r.status >= 400, "{}", r.status);
    std::thread::sleep(Duration::from_secs(3));
    for m in smtp.messages().into_iter().skip(before) {
        assert!(
            m.to.iter().all(|t| t.contains("victim@example.org")),
            "{:?}",
            m.to
        );
        assert!(!m.data.contains("attacker@evil.example"));
    }
}

/// OIDC environments for the account-linking advisories.
struct Oidc {
    s: TestServer,
    idp: MockProvider,
    _dir: tempfile::TempDir,
}

fn oidc(extra_provider: &str, extra_config: &str) -> Oidc {
    let idp = MockProvider::start();
    let dir = tempfile::tempdir().unwrap();
    let secret = dir.path().join("secret");
    std::fs::write(&secret, "s3cret").unwrap();
    let s = TestServer::start_full(
        &[],
        &format!("{}{extra_config}", idp.config(&secret, extra_provider)),
    );
    s.create_user("alice", PASSWORD, false);
    s.db.execute("UPDATE users SET email_verified = true");
    Oidc { s, idp, _dir: dir }
}

fn header() -> serde_json::Value {
    json!({"alg": "ES256", "kid": "test-key"})
}

fn oidc_login(e: &Oidc, c: &mut Client, claims: impl Fn(&str) -> serde_json::Value) -> Resp {
    let r = c.get("/-/login/oidc/mock");
    let (state, nonce) = state_and_nonce(r.header("location").unwrap());
    e.idp
        .next_token(e.idp.sign(&header(), &claims(&nonce), false));
    c.get(&format!("/-/login/oidc/mock/callback?code=c&state={state}"))
}

fn link(e: &Oidc, c: &mut Client, sub: &str) {
    let r = submit(c, "/-/settings/security", "/-/settings/oidc/mock/link", &[]);
    let (state, nonce) = state_and_nonce(r.header("location").unwrap());
    e.idp
        .next_token(e.idp.sign(&header(), &e.idp.claims(sub, &nonce), false));
    let r = c.get(&format!("/-/login/oidc/mock/callback?code=c&state={state}"));
    assert_eq!(r.status, 303, "{}", r.body);
}

/// Avatar fetch from provider claims (Gitea GHSA-x77v-q46j-393g, G51).
#[test]
fn conf_sec_04_oidc_picture_not_fetched() {
    let (port, hits) = listener("HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n".into());
    let e = oidc("allow_registration = true\n", "");
    let mut c = e.s.client();
    let r = oidc_login(&e, &mut c, |nonce| {
        let mut cl = e.idp.claims("pic-sub", nonce);
        cl["picture"] = format!("http://127.0.0.1:{port}/latest/meta-data/").into();
        cl["preferred_username"] = "pic".into();
        cl
    });
    assert_eq!(r.status, 303, "{}", r.body);
    let r = submit(
        &mut c,
        "/-/login/oidc/register",
        "/-/login/oidc/register",
        &[("username", "pic")],
    );
    assert_eq!(r.status, 303, "{}", r.body);
    c.get("/pic");
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(hits.load(Ordering::SeqCst), 0);
}

/// OpenID identity linking takeover and cross-session linking (Gitea
/// GHSA-8xjr-x7rg-hp5h, GHSA-mf69-7pqq-86vx, G52).
#[test]
fn conf_auth_04_oidc_link_requires_existing_factor() {
    let e = oidc("trust_email = true\nallow_registration = true\n", "");
    let mut m = e.s.client();
    let r = oidc_login(&e, &mut m, |nonce| {
        let mut cl = e.idp.claims("mallory-sub", nonce);
        cl["email"] = "alice@example.org".into();
        cl["email_verified"] = true.into();
        cl
    });
    assert_eq!(r.status, 403, "{}", r.body);
    assert_ne!(m.get("/-/settings/profile").status, 200);
    e.s.create_user("mallory", PASSWORD, false);
    let mut mal = e.s.login("mallory", PASSWORD);
    let r = submit(
        &mut mal,
        "/-/settings/security",
        "/-/settings/oidc/mock/link",
        &[],
    );
    let (state, nonce) = state_and_nonce(r.header("location").unwrap());
    e.idp.next_token(
        e.idp
            .sign(&header(), &e.idp.claims("mallory-sub", &nonce), false),
    );
    let mut a = e.s.login("alice", PASSWORD);
    assert_eq!(
        a.get(&format!("/-/login/oidc/mock/callback?code=c&state={state}"))
            .status,
        400
    );
    assert_eq!(
        e.s.db
            .query_one("SELECT count(*)::text FROM oidc_identities"),
        "0"
    );
    // Linking without sudo mode asks for the password first.
    e.s.db.execute("UPDATE sessions SET sudo_until = NULL");
    let r = submit(
        &mut a,
        "/-/settings/security",
        "/-/settings/oidc/mock/link",
        &[],
    );
    assert!(
        r.header("location").unwrap().starts_with("/-/sudo"),
        "{:?}",
        r.header("location")
    );
}

/// Second-factor bypass through alternative sign-in paths (Gitea
/// GHSA-… 2FA bypass via OpenID, G53/G55): password and OIDC sign-ins both
/// stop at the second factor (resets: recovery.rs).
#[test]
fn conf_auth_02_mfa_enforced_on_every_login_path() {
    let e = oidc("", "");
    let mut a = e.s.login("alice", PASSWORD);
    link(&e, &mut a, "alice-sub");
    e.s.db
        .execute("INSERT INTO encryption_keys (key_id) VALUES ('0badc0de') ON CONFLICT DO NOTHING");
    e.s.db.execute("INSERT INTO totp_credentials (user_id, key_id, secret_enc, confirmed) SELECT id, '0badc0de', '\\x00', true FROM users");
    let r = slath_tests::http::login(&mut e.s.client(), "alice", PASSWORD);
    assert!(
        r.header("location")
            .unwrap()
            .starts_with("/-/login/two-factor")
    );
    let mut c = e.s.client();
    let r = oidc_login(&e, &mut c, |n| e.idp.claims("alice-sub", n));
    assert!(
        r.header("location")
            .unwrap()
            .starts_with("/-/login/two-factor"),
        "{:?}",
        r.header("location")
    );
    assert_ne!(c.get("/-/settings/profile").status, 200);
}

/// Disabled accounts reactivated by external sign-in (Gitea G54).
#[test]
fn conf_auth_03_disabled_account_stays_disabled() {
    let e = oidc("allow_registration = true\n", "");
    let mut a = e.s.login("alice", PASSWORD);
    link(&e, &mut a, "alice-sub");
    e.s.db.execute("UPDATE users SET state = 'suspended'");
    let mut c = e.s.client();
    let r = oidc_login(&e, &mut c, |n| e.idp.claims("alice-sub", n));
    assert_eq!(r.status, 403, "{}", r.body);
    assert_ne!(c.get("/-/settings/profile").status, 200);
    assert_eq!(e.s.db.query_one("SELECT state FROM users"), "suspended");
    assert_eq!(
        e.s.db.query_one("SELECT count(*)::text FROM users"),
        "1",
        "no new account"
    );
}

/// Discovery cache poisoning through forwarded host headers (GitLab
/// CVE-2026-112212, L16): the redirect URI and endpoints come from the
/// configuration only.
#[test]
fn conf_auth_01_oidc_discovery_cache_not_poisonable() {
    let e = oidc("", "");
    let mut c = e.s.client();
    let r = c.get_with(
        "/-/login/oidc/mock",
        &[
            ("host", "evil.example"),
            ("x-forwarded-host", "evil.example"),
            ("x-forwarded-proto", "https"),
        ],
    );
    let loc = r.header("location").unwrap().to_owned();
    assert!(
        loc.starts_with(&format!("{}/authorize?", e.idp.issuer)),
        "{loc}"
    );
    assert!(!loc.contains("evil.example"), "{loc}");
    let base = e.s.base();
    let redirect = format!(
        "redirect_uri={}",
        url_encode(&format!("{base}/-/login/oidc/mock/callback"))
    );
    assert!(loc.contains(&redirect), "{loc}");
    let r = c.get("/-/login/oidc/mock");
    assert!(r.header("location").unwrap().starts_with(&e.idp.issuer));
    // Discovery was fetched once and only from the issuer.
    let discovery = e
        .idp
        .requests()
        .iter()
        .filter(|r| r.contains("openid-configuration"))
        .count();
    assert_eq!(discovery, 1);
}

fn url_encode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
            out.push(char::from(b));
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}
