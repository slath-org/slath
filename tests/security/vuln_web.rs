// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Regression tests derived from advisories in other forges and in Git.
//! Each test names the advisory it guards against.

use slath_http::routes::{Access, route_table};
use slath_tests::http::{Client, Resp, create_token, csrf_token, form_encode, login, submit};
use slath_tests::server::{DEMO, GitClient, PASSWORD, TestServer};

const PW: &str = "correct horse battery staple";

/// Writes `files` into a fresh work tree, commits, and pushes `main`.
fn push_files(s: &TestServer, files: &[(&str, &[u8])]) -> (GitClient, tempfile::TempDir) {
    let git = GitClient::new();
    let work = tempfile::tempdir().unwrap();
    let src = work.path().join("src");
    std::fs::create_dir(&src).unwrap();
    git.ok(&src, &["init", "-q"]);
    for (name, data) in files {
        let p = src.join(name);
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(p, data).unwrap();
    }
    git.ok(&src, &["add", "-A"]);
    git.ok(&src, &["commit", "-q", "-m", "files"]);
    git.ok(&src, &["push", "-q", &s.url(&DEMO), "HEAD:refs/heads/main"]);
    (git, work)
}

fn header<'a>(r: &'a Resp, name: &str) -> &'a str {
    r.header(name).unwrap_or("")
}

/// The headers every raw response for active content must carry.
fn assert_inert_download(r: &Resp, what: &str) {
    assert_eq!(r.status, 200, "{what}");
    assert_eq!(
        header(r, "content-type"),
        "application/octet-stream",
        "{what}"
    );
    assert!(
        header(r, "content-disposition").starts_with("attachment"),
        "{what}"
    );
    assert_eq!(header(r, "x-content-type-options"), "nosniff", "{what}");
    assert!(
        header(r, "content-security-policy").starts_with("sandbox"),
        "{what}"
    );
}

const PNG: &[u8] =
    b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR\0\0\0\x01\0\0\0\x01\x08\x06\0\0\0\x1f\x15\xc4\x89";

fn raw_fixture() -> Vec<(&'static str, &'static [u8])> {
    vec![
        ("x.svg", b"<svg xmlns=\"http://www.w3.org/2000/svg\"><script>alert(1)</script></svg>"),
        ("x.html", b"<!doctype html><html><body><script>alert(1)</script></body></html>"),
        ("x.xhtml", b"<html xmlns=\"http://www.w3.org/1999/xhtml\"><script>alert(1)</script></html>"),
        ("x.xml", b"<?xml version=\"1.0\"?><x:script xmlns:x=\"http://www.w3.org/1999/xhtml\">alert(1)</x:script>"),
        ("x.pdf", b"%PDF-1.4\n%\xe2\xe3\xcf\xd3\n1 0 obj << /JS (app.alert(1)) >> endobj\n"),
        ("fake.png", b"<html><body><script>alert(1)</script></body></html>"),
        ("noext", b"   <html><img src=x onerror=alert(1)></html>"),
        ("x.svg;.png", b"<svg onload=alert(1)></svg>"),
        ("real.png", PNG),
        ("notes.txt", b"plain text\n"),
    ]
}

/// Gitea G61 (container blob served with client-chosen type), Gogs/Gitea raw
/// SVG and HTML XSS (CVE-2021-45330 family): raw files never render as active
/// content on the forge's origin.
#[test]
fn conf_sec_04_raw_svg_and_html() {
    let s = TestServer::start();
    push_files(&s, &raw_fixture());
    let mut c = s.login("alice", PASSWORD);
    for name in [
        "x.svg", "x.html", "x.xhtml", "x.xml", "x.pdf", "fake.png", "noext",
    ] {
        let r = c.get(&format!("/alice/demo/raw/main/{name}"));
        assert_inert_download(&r, name);
    }
    let r = c.get("/alice/demo/raw/main/real.png");
    assert_eq!(r.status, 200);
    assert_eq!(header(&r, "content-type"), "image/png");
    assert_eq!(header(&r, "x-content-type-options"), "nosniff");
    assert!(header(&r, "content-security-policy").starts_with("sandbox"));
    let r = c.get("/alice/demo/raw/main/notes.txt");
    assert_eq!(header(&r, "content-type"), "text/plain; charset=utf-8");
    assert_eq!(header(&r, "x-content-type-options"), "nosniff");
    assert!(header(&r, "content-security-policy").starts_with("sandbox"));
    // The download name can't break out of the header.
    let r = c.get("/alice/demo/raw/main/x.svg;.png");
    assert_inert_download(&r, "x.svg;.png");
    let cd = header(&r, "content-disposition");
    assert_eq!(cd.matches(';').count(), 1, "{cd}");
}

/// Gitea G61 / GitHub Enterprise raw content-type confusion: no request
/// parameter, `Accept` header, or file-name trick picks the response type.
#[test]
fn conf_sec_04_raw_content_type_not_from_client() {
    let s = TestServer::start();
    push_files(&s, &raw_fixture());
    let mut c = s.login("alice", PASSWORD);
    let variants: [(&str, &[(&str, &str)]); 6] = [
        ("/alice/demo/raw/main/x.svg?content_type=text/html", &[]),
        ("/alice/demo/raw/main/x.svg?type=image/svg%2Bxml", &[]),
        ("/alice/demo/raw/main/x.svg", &[("accept", "text/html")]),
        ("/alice/demo/raw/main/x.svg", &[("accept", "image/svg+xml")]),
        (
            "/alice/demo/raw/main/x.svg;.png",
            &[("accept", "image/png")],
        ),
        (
            "/alice/demo/raw/main/x.html?download=0&inline=1",
            &[("accept", "text/html")],
        ),
    ];
    for (path, headers) in variants {
        let r = c.get_with(path, headers);
        assert_inert_download(&r, path);
    }
    // A text file stays text whatever is asked for.
    let r = c.get_with(
        "/alice/demo/raw/main/notes.txt?content_type=text/html",
        &[("accept", "text/html")],
    );
    assert_eq!(header(&r, "content-type"), "text/plain; charset=utf-8");
}

const PAYLOADS: &[&str] = &[
    "<img src=x onerror=alert(1)>",
    "\"><svg/onload=alert(1)>",
    "'-alert(1)-'",
    "javascript:alert(1)",
    "{{7*7}}",
    "</script><script>alert(1)</script>",
    "hx-get=\"/-/logout\" hx-trigger=\"load\"",
    "\u{202e}gpj.exe",
];

/// Fragments that only appear if a payload was rendered unescaped.
const UNESCAPED: &[&str] = &[
    "<img src=x onerror",
    "<svg/onload",
    "<svg onload",
    "<script>alert",
    "hx-trigger=\"load\"",
    "href=\"javascript:",
    "src=\"javascript:",
    "=49",
];

/// Gitea CVE-2025-68942 (XSS via names in the repository view), GitHub
/// Enterprise Server CVE-2025-11892 (XSS via branch names), and the general
/// family of stored XSS through file, ref, and display names: every page that
/// shows such a name escapes it, and no htmx attribute can be smuggled in.
#[test]
fn conf_sec_04_xss_names_payload_matrix() {
    let s = TestServer::start();
    let mut files: Vec<(String, Vec<u8>)> = vec![
        ("README.md".into(), b"# readme\n".to_vec()),
        ("<img src=x onerror=alert(1)>.md".into(), b"x\n".to_vec()),
        ("\"><svg onload=alert(1)>/inner.txt".into(), b"x\n".to_vec()),
    ];
    for (i, p) in PAYLOADS.iter().enumerate() {
        files.push((format!("{}-{i}.txt", p.replace('/', "_")), b"x\n".to_vec()));
    }
    let git = GitClient::new();
    let work = tempfile::tempdir().unwrap();
    let src = work.path().join("src");
    std::fs::create_dir(&src).unwrap();
    git.ok(&src, &["init", "-q"]);
    for (name, data) in &files {
        let p = src.join(name);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, data).unwrap();
    }
    git.ok(&src, &["add", "-A"]);
    let message = format!(
        "subject {}\n\nbody {}\n",
        PAYLOADS.join(" "),
        PAYLOADS.join("\n")
    );
    git.ok(
        &src,
        &[
            "-c",
            "user.name=Mallory <img src=x onerror=alert(1)> \"><svg/onload=alert(1)>",
            "-c",
            "user.email=x\"onmouseover=alert(1)@example.org",
            "commit",
            "-q",
            "-m",
            &message,
        ],
    );
    let head = git.ok(&src, &["rev-parse", "HEAD"]).trim().to_owned();
    let branch = "\"><svg/onload=alert(1)>";
    let tag = "'-alert(1)-'";
    git.ok(&src, &["tag", tag]);
    git.ok(
        &src,
        &[
            "push",
            "-q",
            &s.url(&DEMO),
            "HEAD:refs/heads/main",
            &format!("HEAD:refs/heads/{branch}"),
            &format!("refs/tags/{tag}:refs/tags/{tag}"),
        ],
    );

    let mut a = s.login("alice", PASSWORD);
    let name = PAYLOADS[..2].join(" ");
    let r = submit(
        &mut a,
        "/-/settings/profile",
        "/-/settings/profile",
        &[("display_name", &name)],
    );
    assert!(r.status < 400, "{}", r.body);
    let r = submit(
        &mut a,
        "/-/orgs/new",
        "/-/orgs",
        &[("name", "acme"), ("display_name", &name)],
    );
    assert_eq!(r.status, 303, "{}", r.body);
    // Descriptions refuse text-direction controls; the rest must be escaped.
    let desc = PAYLOADS[..7].join(" ");
    let r = submit(
        &mut a,
        "/alice/demo/settings/general",
        "/alice/demo/settings/general",
        &[("description", &desc), ("default_branch", "main")],
    );
    assert!(r.status < 400, "{}", r.body);
    create_token(&mut a, &PAYLOADS[..2].join(""), &["user:read"]);

    let enc = |s: &str| {
        s.bytes()
            .map(|b| {
                if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
                    (b as char).to_string()
                } else {
                    format!("%{b:02X}")
                }
            })
            .collect::<String>()
    };
    let pages = vec![
        "/".to_owned(),
        "/alice".to_owned(),
        "/acme".to_owned(),
        "/acme/-/members".to_owned(),
        "/alice/demo".to_owned(),
        "/alice/demo/tree/main".to_owned(),
        format!("/alice/demo/tree/main/{}", enc("\"><svg onload=alert(1)>")),
        format!(
            "/alice/demo/blob/main/{}",
            enc("<img src=x onerror=alert(1)>.md")
        ),
        "/alice/demo/commits/main".to_owned(),
        format!("/alice/demo/commits/{}", enc(branch)),
        format!("/alice/demo/tree/{}", enc(branch)),
        format!("/alice/demo/commit/{head}"),
        "/alice/demo/branches".to_owned(),
        "/alice/demo/tags".to_owned(),
        "/alice/demo/settings/general".to_owned(),
        "/-/settings/profile".to_owned(),
        "/-/settings/tokens".to_owned(),
    ];
    let mut failures = Vec::new();
    for p in &pages {
        let r = a.get(p);
        if r.status != 200 {
            failures.push(format!("{p}: status {}", r.status));
            continue;
        }
        // Rendered user content (README) is sanitized separately; check the
        // page's own markup.
        for bad in UNESCAPED {
            if r.body.contains(bad) {
                failures.push(format!("{p}: unescaped {bad:?}"));
            }
        }
    }
    // The payloads were really shown (escaped), not dropped.
    let tree = a.get("/alice/demo/tree/main").body;
    assert!(tree.contains("{{7*7}}"), "template syntax shown literally");
    assert!(
        tree.contains("&#60;img src=x onerror=alert(1)&#62;")
            || tree.contains("&lt;img src=x onerror=alert(1)&gt;")
    );
    let home = a.get("/alice/demo").body;
    assert!(
        home.contains("javascript:alert(1)"),
        "description shown as text"
    );
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// Fills a route template with values that exist on the test server.
fn concrete(template: &str, head: &str) -> String {
    template
        .split('/')
        .map(|seg| match seg {
            "{ns}" | "{name}" | "{user}" => "alice".to_owned(),
            "{repo}" => "demo".to_owned(),
            "{org}" => "acme".to_owned(),
            "{op}" if template.contains("/settings/state/") => "archive".to_owned(),
            "{op}" => "suspend".to_owned(),
            "{id}" => "0192f1a2-3b4c-7d5e-8f60-718293a4b5c6".to_owned(),
            "{what}" => "labels".to_owned(),
            "{file}" => "alice.svg".to_owned(),
            "{hash}" => "0".to_owned(),
            "{ref}" => "main".to_owned(),
            "{oid}" | "{rest}" => head.to_owned(),
            "{*rest}" if template.contains("/archive/") => "main.tar.gz".to_owned(),
            "{*rest}" => "main/README.md".to_owned(),
            "{*file}" => "app.js".to_owned(),
            s => s.to_owned(),
        })
        .collect::<Vec<_>>()
        .join("/")
}

/// (description, extra headers, body).
type Variant<'a> = (&'a str, Vec<(&'a str, String)>, Option<(&'a str, Vec<u8>)>);

fn state_fingerprint(s: &TestServer) -> String {
    [
        "users",
        "namespaces",
        "organizations",
        "org_members",
        "repositories",
        "repo_collaborators",
        "repo_redirects",
        "access_tokens",
        "ssh_keys",
    ]
    .iter()
    .map(|t| {
        s.db.query_one(&format!(
            "SELECT coalesce(md5(string_agg(x::text, ',' ORDER BY x::text)), '-') FROM {t} x"
        ))
    })
    .collect::<Vec<_>>()
    .join(" ")
}

/// Gitea CSRF on state-changing GET/POST routes (G2, G56) and the family of
/// login-CSRF and missing-token bugs: every cookie-authenticated
/// state-changing UI route refuses requests without a same-site origin and
/// this session's token, and nothing changes.
#[test]
fn conf_sec_04_csrf_matrix() {
    let s = TestServer::start();
    let (git, work) = push_files(&s, &[("README.md", b"# x\n")]);
    let head = git
        .ok(&work.path().join("src"), &["rev-parse", "HEAD"])
        .trim()
        .to_owned();
    s.create_user("bob", PW, false);
    s.db.execute(
        "UPDATE users SET is_admin = true WHERE namespace_id = (SELECT id FROM namespaces WHERE name = 'alice')",
    );
    let mut a = s.login("alice", PASSWORD);
    let r = submit(
        &mut a,
        "/-/orgs/new",
        "/-/orgs",
        &[("name", "acme"), ("display_name", "")],
    );
    assert_eq!(r.status, 303);
    let token = csrf_token(&a.get("/-/settings/profile").body).unwrap();
    let mut b = s.login("bob", PW);
    let bob_token = csrf_token(&b.get("/-/settings/profile").body).unwrap();
    let before = state_fingerprint(&s);

    let fields = |t: Option<&str>| {
        let mut f = vec![
            ("confirm", "alice/demo"),
            ("name", "renamed"),
            ("display_name", "pwned"),
            ("role", "admin"),
            ("username", "bob"),
            ("visibility", "public"),
            ("new_password", "attacker password 123"),
            ("confirm_password", "attacker password 123"),
            ("title", "k"),
            (
                "key",
                "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIOvPv0jsKaS3dwgYaEOtYHhDVLiOtMLgSNVxQEUz3dSc",
            ),
        ];
        if let Some(t) = t {
            f.push(("_csrf", t));
        }
        form_encode(&f).into_bytes()
    };
    let form = "application/x-www-form-urlencoded";
    let mut checked = 0;
    let mut failures = Vec::new();
    for (template, access) in route_table() {
        if template.starts_with("/api/")
            || matches!(access, Access::Git)
            || [
                "/-/healthz",
                "/-/readyz",
                "/-/csp-report",
                "/metrics",
                "/-/design",
            ]
            .contains(&template)
        {
            continue;
        }
        let path = concrete(template, &head);
        let variants: Vec<Variant> = vec![
            ("no token", vec![], Some((form, fields(None)))),
            (
                "other session's token",
                vec![],
                Some((form, fields(Some(&bob_token)))),
            ),
            (
                "other session's header token",
                vec![("x-csrf-token", bob_token.clone())],
                Some((form, fields(None))),
            ),
            (
                "wrong token",
                vec![],
                Some((
                    form,
                    fields(Some("AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA")),
                )),
            ),
            (
                "cross-site fetch",
                vec![("sec-fetch-site", "cross-site".to_owned())],
                Some((form, fields(Some(&token)))),
            ),
            (
                "same-site fetch",
                vec![("sec-fetch-site", "same-site".to_owned())],
                Some((form, fields(Some(&token)))),
            ),
            (
                "foreign origin",
                vec![("origin", "https://evil.example".to_owned())],
                Some((form, fields(Some(&token)))),
            ),
            (
                "null origin",
                vec![("origin", "null".to_owned())],
                Some((form, fields(Some(&token)))),
            ),
            ("no origin", vec![], Some((form, fields(Some(&token))))),
            (
                "text/plain form",
                vec![],
                Some(("text/plain", fields(Some(&token)))),
            ),
            (
                "json body",
                vec![],
                Some((
                    "application/json",
                    format!("{{\"_csrf\":\"{token}\"}}").into_bytes(),
                )),
            ),
        ];
        for (what, headers, body) in variants {
            // Origin checks: drop the client's default same-origin headers
            // for the variants that test them.
            let saved = a.default_headers.clone();
            if matches!(what, "foreign origin" | "null origin" | "no origin") {
                a.default_headers.clear();
            } else if what.ends_with("fetch") {
                a.default_headers.retain(|(k, _)| k != "sec-fetch-site");
            }
            let hs: Vec<(&str, &str)> = headers.iter().map(|(k, v)| (*k, v.as_str())).collect();
            let r = a.request("POST", &path, &hs, body);
            a.default_headers = saved;
            match r.status {
                403 => checked += 1,
                // POST isn't routed here at all.
                405 => {}
                st => failures.push(format!("{what}: POST {path} ({access:?}) -> {st}")),
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
    assert!(checked > 300, "only {checked} rejections observed");
    assert_eq!(
        state_fingerprint(&s),
        before,
        "a rejected request changed state"
    );
    // Still signed in (the logout POSTs were refused), and GET never mutates.
    assert!(a.get("/").body.contains("action=\"/-/logout\""));
    assert_ne!(a.get("/-/logout").status, 200);
    assert!(
        a.get("/").body.contains("action=\"/-/logout\""),
        "GET /-/logout signed out"
    );
}

fn assert_local_redirect(r: &Resp, what: &str) {
    if !(300..400).contains(&r.status) {
        return;
    }
    let loc = header(r, "location");
    let ok = loc.starts_with('/')
        && !loc.starts_with("//")
        && !loc.starts_with("/\\")
        && !loc.bytes().any(|b| b.is_ascii_control() || b == b'\\');
    assert!(ok, "{what}: redirect to {loc:?}");
    let lower = loc.to_ascii_lowercase();
    assert!(
        !lower.contains("evil.com") || lower.starts_with("/.evil.com"),
        "{what}: {loc:?}"
    );
    assert!(
        !r.headers.contains_key("x") && !header(r, "set-cookie").contains("x=y"),
        "{what}: header injection"
    );
}

/// Gitea open redirects via `redirect_to` (G57, CVE-2022-1928 family) and
/// header injection through CRLF in redirect targets: `next` after sign-in,
/// sudo, or on the login page never leaves the site.
#[test]
fn conf_sec_04_open_redirect_variants() {
    let s = TestServer::start();
    let host = s.base().trim_start_matches("http://").to_owned();
    let raw_targets = [
        "//evil.com".to_owned(),
        "/\\evil.com".to_owned(),
        "/%2F%2Fevil.com".to_owned(),
        "/%2f%2fevil.com".to_owned(),
        "/%5Cevil.com".to_owned(),
        "https://evil.com".to_owned(),
        "https:evil.com".to_owned(),
        "/\t/evil.com".to_owned(),
        "\\\\evil.com".to_owned(),
        "/.evil.com".to_owned(),
        format!("//{host}@evil.com"),
        "javascript:alert(1)".to_owned(),
        "/%0d%0aSet-Cookie:x=y".to_owned(),
        "/\r\nSet-Cookie: x=y".to_owned(),
        "///evil.com".to_owned(),
        " //evil.com".to_owned(),
        "\u{3002}evil.com".to_owned(),
    ];
    for target in &raw_targets {
        let q = form_encode(&[("next", target.as_str())]);
        // Sign-in form.
        let mut c = s.client();
        let page = c.get(&format!("/-/login?{q}"));
        assert!(page.status < 500, "{target:?}");
        assert!(
            !page.body.contains("value=\"//evil") && !page.body.contains("value=\"javascript:"),
            "{target:?}"
        );
        let token = csrf_token(&page.body).unwrap();
        let r = c.post_form(
            "/-/login",
            &[
                ("_csrf", &token),
                ("username", "alice"),
                ("password", PASSWORD),
                ("next", target),
            ],
        );
        assert_eq!(r.status, 303, "{target:?}: {}", r.body);
        assert_local_redirect(&r, &format!("login next={target:?}"));
        // Already signed in: the login page redirects to `next`.
        let r = c.get(&format!("/-/login?{q}"));
        assert_local_redirect(&r, &format!("signed-in login page next={target:?}"));
        // Sudo.
        s.db.execute("UPDATE sessions SET sudo_until = NULL");
        let page = c.get(&format!("/-/sudo?{q}"));
        assert!(page.status < 500);
        let token = csrf_token(&page.body).unwrap();
        let r = c.post_form(
            "/-/sudo",
            &[("_csrf", &token), ("password", PASSWORD), ("next", target)],
        );
        assert_local_redirect(&r, &format!("sudo next={target:?}"));
        // Sign-out goes home.
        let r = c.post_form("/-/logout", &[("_csrf", &token), ("next", target)]);
        assert_local_redirect(&r, &format!("logout next={target:?}"));
    }
}

/// Gitea's reverse-proxy header trust (G59, G60: `X-Forwarded-For` spoofing
/// around rate limits, `X-WEBAUTH-USER` header authentication): headers from
/// peers outside `trusted_proxies` change nothing.
#[test]
fn conf_sec_04_untrusted_forwarded_headers() {
    let s = TestServer::start_opts(
        &[DEMO],
        "",
        &[
            ("SLATH__SERVER__TRUSTED_PROXIES", "[\"10.255.255.0/24\"]"),
            ("SLATH__LIMITS__LOGIN_FAILURES_PER_IP", "3"),
        ],
    );
    let spoof = |i: usize| {
        vec![
            ("x-forwarded-for".to_owned(), format!("1.2.3.{i}")),
            ("x-real-ip".to_owned(), format!("1.2.3.{i}")),
            (
                "forwarded".to_owned(),
                format!("for=1.2.3.{i};proto=https;host=evil.com"),
            ),
            ("x-forwarded-proto".to_owned(), "https".to_owned()),
            ("x-forwarded-host".to_owned(), "evil.com".to_owned()),
            ("x-remote-user".to_owned(), "alice".to_owned()),
            ("x-webauth-user".to_owned(), "alice".to_owned()),
            ("remote-user".to_owned(), "alice".to_owned()),
        ]
    };
    // No header-based authentication.
    let mut anon = s.client();
    anon.default_headers.extend(spoof(9));
    let r = anon.get("/-/settings/profile");
    assert_ne!(r.status, 200, "header authentication must not exist");
    assert!(!r.body.contains("display_name"));
    assert_eq!(anon.get("/alice/demo").status, 404);
    // Generated URLs use public_url, never the forwarded host or scheme.
    let mut a = s.client();
    a.default_headers.extend(spoof(10));
    assert_eq!(login(&mut a, "alice", PASSWORD).status, 303);
    let page = a.get("/alice/demo").body;
    assert!(!page.contains("evil.com"), "forwarded host used in links");
    assert!(page.contains(&format!("{}/alice/demo.git", s.base())));
    // The cookie follows public_url (http here), not X-Forwarded-Proto.
    let mut c = s.client();
    c.default_headers.extend(spoof(11));
    let r = login(&mut c, "alice", PASSWORD);
    let cookies = r.set_cookies().join("\n");
    assert!(!cookies.contains("__Host-"), "{cookies}");
    // The audit log records the socket address.
    let ip = s.db.query_one(
        "SELECT host(actor_ip) FROM audit_events WHERE action = 'login.success' ORDER BY created_at DESC LIMIT 1",
    );
    assert_eq!(ip, "127.0.0.1");
    // Throttling is keyed on the socket address: rotating X-Forwarded-For
    // doesn't buy more attempts.
    for i in 0..3 {
        let mut c = s.client();
        c.default_headers.extend(spoof(20 + i));
        let r = login(&mut c, &format!("nobody{i}"), "wrong password!!");
        assert_eq!(r.status, 401, "attempt {i}");
    }
    let mut c = s.client();
    c.default_headers.extend(spoof(99));
    assert_eq!(login(&mut c, "nobody9", "wrong password!!").status, 429);
}

/// Gitea/Gogs web installers reachable after setup (G58; Gogs install-page
/// takeover): there is no web setup endpoint, before or after the first
/// account exists. The first administrator is created with the CLI.
#[test]
fn conf_sec_04_no_web_setup_endpoint() {
    let s = TestServer::start_with(&[]);
    let paths = [
        "/install",
        "/install/",
        "/setup",
        "/-/setup",
        "/-/install",
        "/admin/install",
        "/-/admin/setup",
        "/-/admin/install",
        "/install.php",
        "/api/v1/setup",
        "/api/v1/install",
        "/-/bootstrap",
        "/-/first-run",
    ];
    let check = |c: &mut Client, phase: &str| {
        for p in paths {
            let r = c.get(p);
            assert_eq!(r.status, 404, "{phase} GET {p}");
            let token = csrf_token(&c.get("/-/login").body).unwrap();
            let body = form_encode(&[
                ("_csrf", token.as_str()),
                ("username", "takeover"),
                ("password", "takeover password 123"),
                ("email", "x@example.org"),
                ("admin", "1"),
            ]);
            let r = c.request(
                "POST",
                p,
                &[],
                Some(("application/x-www-form-urlencoded", body.into_bytes())),
            );
            assert!(
                matches!(r.status, 401 | 403 | 404 | 405),
                "{phase} POST {p}: {}",
                r.status
            );
        }
        assert_eq!(
            s.db.query_one("SELECT count(*)::text FROM namespaces WHERE name = 'takeover'"),
            "0",
            "{phase}"
        );
    };
    assert_eq!(s.db.query_one("SELECT count(*)::text FROM users"), "0");
    check(&mut s.client(), "empty instance");
    s.create_user("alice", PW, true);
    check(&mut s.client(), "populated instance");
    // Anonymous API calls can't create accounts either.
    let r = Client::new(&s.base()).api(
        "POST",
        "/api/v1/users",
        None,
        Some(&serde_json::json!({"username": "takeover", "password": "x", "admin": true})),
    );
    assert!(matches!(r.status, 401 | 404 | 405), "{}", r.status);
}
