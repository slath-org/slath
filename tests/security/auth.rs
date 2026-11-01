// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Sign-in, sessions, and CSRF.

use slath_tests::http::{Client, csrf_token, login, submit};
use slath_tests::server::TestServer;

const PW: &str = "correct horse battery staple";

fn session_cookie(c: &Client) -> Option<String> {
    c.cookies
        .get("slath_session")
        .or_else(|| c.cookies.get("__Host-slath_session"))
        .cloned()
}

fn signed_in_as(c: &mut Client, name: &str) -> bool {
    let home = c.get("/");
    home.body.contains(&format!("href=\"/{name}\">{name}</a>"))
}

/// The page with values that differ per response (nonces, tokens, IDs) blanked.
fn normalized(body: &str) -> String {
    let mut out = String::new();
    let mut rest = body;
    while let Some(i) = rest.find(['"', '>']) {
        out.push_str(&rest[..=i]);
        rest = &rest[i + 1..];
        let end = rest.find(['"', '<']).unwrap_or(rest.len());
        let value = &rest[..end];
        let random = value.len() >= 20
            && value
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"+/=_-".contains(&b));
        out.push_str(if random { "*" } else { value });
        rest = &rest[end..];
    }
    out.push_str(rest);
    out
}

#[test]
fn conf_sec_04_session_id_rotates_on_login() {
    let s = TestServer::start_with(&[]);
    s.create_user("alice", PW, false);
    let mut c = s.client();
    // An attacker-chosen session ID is never adopted (fixation).
    let planted = "A".repeat(43);
    c.cookies.insert("slath_session".into(), planted.clone());
    assert_eq!(login(&mut c, "alice", PW).status, 303);
    let first = session_cookie(&c).unwrap();
    assert_ne!(first, planted);
    assert!(signed_in_as(&mut c, "alice"));
    // Signing in again issues a new ID.
    let r = submit(
        &mut c,
        "/-/design",
        "/-/login",
        &[("username", "alice"), ("password", PW)],
    );
    assert_eq!(r.status, 303);
    let second = session_cookie(&c).unwrap();
    assert_ne!(first, second);
}

#[test]
fn conf_sec_04_session_cookie_flags() {
    // An http instance (development): no Secure, no __Host- prefix.
    let s = TestServer::start_with(&[]);
    s.create_user("alice", PW, false);
    let mut c = s.client();
    let r = login(&mut c, "alice", PW);
    let cookie = r
        .set_cookies()
        .into_iter()
        .find(|l| l.starts_with("slath_session="))
        .unwrap()
        .to_owned();
    for attr in ["HttpOnly", "SameSite=Lax", "Path=/"] {
        assert!(cookie.contains(attr), "{cookie}");
    }
    assert!(!cookie.contains("Domain="), "{cookie}");

    // An https instance: __Host- prefix and Secure.
    let s = TestServer::start_opts(
        &[],
        "",
        &[("SLATH__SERVER__PUBLIC_URL", "https://git.example.org")],
    );
    s.create_user("bob", PW, false);
    let mut c = s.client();
    c.default_headers.retain(|(k, _)| k != "origin");
    let r = login(&mut c, "bob", PW);
    assert_eq!(r.status, 303, "{}", r.body);
    let cookie = r
        .set_cookies()
        .into_iter()
        .find(|l| l.starts_with("__Host-slath_session="))
        .unwrap()
        .to_owned();
    for attr in ["Secure", "HttpOnly", "SameSite=Lax", "Path=/"] {
        assert!(cookie.contains(attr), "{cookie}");
    }
    assert!(!cookie.contains("Domain="), "{cookie}");
    let session = cookie.split(';').next().unwrap().split_once('=').unwrap().1;
    assert_eq!(session.len(), 43, "32 random bytes, base64url");
}

#[test]
fn conf_sec_04_cross_site_post_rejected() {
    let s = TestServer::start_with(&[]);
    s.create_user("alice", PW, false);
    let mut c = s.login("alice", PW);
    let token = csrf_token(&c.get("/").body).unwrap();
    let other = {
        let mut o = s.login("alice", PW);
        csrf_token(&o.get("/").body).unwrap()
    };
    let evil = "https://evil.example";
    type Case<'a> = (&'a str, Vec<(&'a str, &'a str)>, String);
    let cases: Vec<Case> = vec![
        (
            "cross-site fetch metadata",
            vec![("sec-fetch-site", "cross-site")],
            token.clone(),
        ),
        (
            "same-site is not same-origin",
            vec![("sec-fetch-site", "same-site")],
            token.clone(),
        ),
        ("no token", vec![], String::new()),
        ("token of another session", vec![], other),
        (
            "foreign origin without fetch metadata",
            vec![("sec-fetch-site", ""), ("origin", evil)],
            token.clone(),
        ),
        (
            "null origin without fetch metadata",
            vec![("sec-fetch-site", ""), ("origin", "null")],
            token.clone(),
        ),
    ];
    for (what, headers, tok) in cases {
        let mut h: Vec<(&str, &str)> = headers;
        let filtered: Vec<(&str, &str)> = h.drain(..).filter(|(_, v)| !v.is_empty()).collect();
        // Remove default headers the case wants absent.
        let saved = c.default_headers.clone();
        if filtered.iter().all(|(k, _)| *k != "sec-fetch-site")
            && what.contains("without fetch metadata")
        {
            c.default_headers.retain(|(k, _)| k != "sec-fetch-site");
        }
        let r = c.post_form_with("/-/logout", &[("_csrf", &tok)], &filtered);
        c.default_headers = saved;
        assert_eq!(r.status, 403, "{what}: {}", r.body);
        assert!(
            signed_in_as(&mut c, "alice"),
            "{what}: the session must survive"
        );
    }
    // A text/plain body (what a cross-site <form enctype=text/plain> sends).
    let r = c.request(
        "POST",
        "/-/logout",
        &[],
        Some(("text/plain", format!("_csrf={token}").into_bytes())),
    );
    assert_eq!(r.status, 403);
    // The real form works.
    let r = c.post_form("/-/logout", &[("_csrf", &token)]);
    assert_eq!(r.status, 303);
    assert!(!signed_in_as(&mut c, "alice"));
}

#[test]
fn conf_auth_02_eleventh_failed_login_is_throttled() {
    let s = TestServer::start_with(&[]);
    s.create_user("alice", PW, false);
    let mut c = s.client();
    for i in 0..10 {
        let r = login(&mut c, "alice", "wrong password!!");
        assert_eq!(r.status, 401, "attempt {i}");
    }
    // Throttled even with the right password, and generic.
    let r = login(&mut c, "alice", PW);
    assert_eq!(r.status, 429, "{}", r.body);
    assert!(!r.body.contains("alice@"), "no account details");
    // Unknown accounts are throttled the same way (no enumeration).
    for _ in 0..10 {
        login(&mut c, "nobody", "wrong password!!");
    }
    assert_eq!(login(&mut c, "nobody", "wrong password!!").status, 429);
}

#[test]
fn conf_auth_02_unknown_user_and_wrong_password_look_identical() {
    let s = TestServer::start_with(&[]);
    s.create_user("alice", PW, false);
    s.create_user("suspended", PW, false);
    s.db.execute("UPDATE users SET state = 'suspended' WHERE id = (SELECT u.id FROM users u JOIN namespaces n ON n.id = u.namespace_id WHERE n.name = 'suspended')");
    let attempt = |name: &str, pw: &str| {
        let mut c = s.client();
        let r = login(&mut c, name, pw);
        (r.status, normalized(&r.body).replace(name, "NAME"))
    };
    let wrong = attempt("alice", "wrong password!!");
    let unknown = attempt("nosuchuser", "wrong password!!");
    let suspended = attempt("suspended", PW);
    assert_eq!(wrong.0, 401);
    assert_eq!(wrong, unknown);
    assert_eq!(wrong, suspended);
    assert!(wrong.1.contains("Incorrect username or password."));
}

#[test]
fn conf_auth_02_password_hash_format() {
    let s = TestServer::start_with(&[]);
    s.create_user("alice", PW, true);
    let hash = s.db.query_one(
        "SELECT password_hash FROM users u JOIN namespaces n ON n.id = u.namespace_id WHERE n.name = 'alice'",
    );
    assert!(
        hash.starts_with("$argon2id$v=19$m=19456,t=2,p=1$"),
        "{hash}"
    );
    assert!(!hash.contains(PW));
}

#[test]
fn login_page_explains_bootstrap_only_on_empty_instances() {
    let s = TestServer::start_with(&[]);
    let mut c = s.client();
    assert!(
        c.get("/-/login")
            .body
            .contains("slath admin create-user --admin")
    );
    s.create_user("alice", PW, true);
    assert!(!c.get("/-/login").body.contains("slath admin create-user"));
}

#[test]
fn logout_revokes_the_session_on_the_next_request() {
    let s = TestServer::start_with(&[]);
    s.create_user("alice", PW, false);
    let mut a = s.login("alice", PW);
    let stolen = session_cookie(&a).unwrap();
    let token = csrf_token(&a.get("/").body).unwrap();
    assert_eq!(a.post_form("/-/logout", &[("_csrf", &token)]).status, 303);
    let mut thief = s.client();
    thief.cookies.insert("slath_session".into(), stolen);
    let r = thief.get("/");
    assert!(!r.body.contains("href=\"/alice\">alice</a>"));
    // The dead cookie is cleared.
    assert!(
        r.set_cookies()
            .iter()
            .any(|l| l.starts_with("slath_session=;")),
        "{:?}",
        r.set_cookies()
    );
}

#[test]
fn conf_sec_04_login_redirect_stays_on_site() {
    let s = TestServer::start_with(&[]);
    s.create_user("alice", PW, false);
    for (next, want) in [
        ("/-/design", "/-/design"),
        ("//evil.example", "/"),
        ("https://evil.example/", "/"),
        ("/\\evil.example", "/"),
        ("/%2F%2Fevil.example", "/"),
    ] {
        let mut c = s.client();
        let page = c.get(&format!(
            "/-/login?next={}",
            slath_tests::http::form_encode(&[("x", next)]).trim_start_matches("x=")
        ));
        let token = csrf_token(&page.body).unwrap();
        let r = c.post_form(
            "/-/login",
            &[
                ("_csrf", &token),
                ("username", "alice"),
                ("password", PW),
                ("next", next),
            ],
        );
        assert_eq!(r.status, 303);
        assert_eq!(r.header("location"), Some(want), "next={next}");
    }
}
