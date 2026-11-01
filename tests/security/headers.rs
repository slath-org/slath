// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Security headers per route class, the CSP, nonces, and probes.

use slath_tests::http::Resp;
use slath_tests::server::TestServer;

fn csp(r: &Resp) -> Vec<(String, Vec<String>)> {
    r.header("content-security-policy")
        .unwrap_or_else(|| panic!("no CSP: {:?}", r.headers))
        .split(';')
        .filter_map(|d| {
            let mut parts = d.split_whitespace();
            let name = parts.next()?.to_owned();
            Some((name, parts.map(str::to_owned).collect()))
        })
        .collect()
}

fn nonce_of(r: &Resp) -> String {
    let script_src = csp(r)
        .into_iter()
        .find(|(n, _)| n == "script-src")
        .unwrap()
        .1;
    let n = script_src
        .iter()
        .find_map(|s| s.strip_prefix("'nonce-").and_then(|s| s.strip_suffix('\'')))
        .unwrap();
    n.to_owned()
}

fn assert_html_headers(r: &Resp) {
    assert!(r.header("content-type").unwrap().starts_with("text/html"));
    assert_eq!(r.header("x-content-type-options"), Some("nosniff"));
    assert_eq!(
        r.header("referrer-policy"),
        Some("strict-origin-when-cross-origin")
    );
    assert_eq!(r.header("cross-origin-opener-policy"), Some("same-origin"));
    assert_eq!(
        r.header("permissions-policy"),
        Some("camera=(), microphone=(), geolocation=()")
    );
    assert!(r.header("x-request-id").is_some_and(|v| v.len() == 32));
    // public_url is http in tests, so no HSTS.
    assert!(r.header("strict-transport-security").is_none());
    let d = csp(r);
    for want in [
        "frame-ancestors",
        "base-uri",
        "object-src",
        "form-action",
        "default-src",
    ] {
        assert!(d.iter().any(|(n, _)| n == want), "CSP lacks {want}: {d:?}");
    }
}

#[test]
fn conf_sec_07_csp_lists_no_third_party_hosts() {
    let s = TestServer::start_with(&[]);
    let mut c = s.client();
    for path in ["/", "/-/design", "/no/such/page"] {
        let r = c.get(path);
        for (name, sources) in csp(&r) {
            for src in sources {
                let ok = matches!(src.as_str(), "'self'" | "'none'" | "data:")
                    || (name == "report-uri" && src == "/-/csp-report")
                    || (name == "report-to" && src == "csp")
                    || src.starts_with("'nonce-")
                    // User-content images (ui.external_images) are the only
                    // permitted cross-origin load, and only as a scheme.
                    || (name == "img-src" && src == "https:");
                assert!(ok, "{path}: {name} allows {src}");
            }
        }
    }
}

#[test]
fn conf_sec_04_html_responses_carry_security_headers() {
    let s = TestServer::start_with(&[]);
    let mut c = s.client();
    let home = c.get("/");
    assert_eq!(home.status, 200);
    assert_html_headers(&home);
    let missing = c.get("/-/no-such-route");
    assert_eq!(missing.status, 404);
    assert_html_headers(&missing);
    assert!(missing.body.contains("<h1>Not found</h1>"));
}

#[test]
fn conf_sec_04_nonce_is_fresh_per_response_and_stamped() {
    let s = TestServer::start_with(&[]);
    let mut c = s.client();
    let a = c.get("/-/design");
    let b = c.get("/-/design");
    let (na, nb) = (nonce_of(&a), nonce_of(&b));
    assert_ne!(na, nb);
    assert!(na.len() >= 22, "nonce must carry 128 bits");
    assert!(a.body.contains(&format!("nonce=\"{na}\"")));
    assert!(a.body.contains(&format!("hx-nonce=\"{na}\"")));
    assert!(!a.body.contains(&nb));
}

#[test]
fn conf_sec_04_non_html_responses_are_locked_down() {
    let s = TestServer::start_with(&[]);
    let mut c = s.client();
    let page = c.get("/");
    let script = page
        .body
        .split("src=\"")
        .nth(1)
        .and_then(|r| r.split('"').next())
        .unwrap()
        .to_owned();
    assert!(script.starts_with("/-/assets/"), "{script}");
    let asset = c.get(&script);
    assert_eq!(asset.status, 200);
    assert_eq!(asset.header("x-content-type-options"), Some("nosniff"));
    assert!(asset.header("cache-control").unwrap().contains("immutable"));
    assert!(
        asset
            .header("content-type")
            .unwrap()
            .starts_with("text/javascript")
    );
    // Assets are only served under their current content hash.
    let wrong = c.get("/-/assets/000000000000/app.js");
    assert_eq!(wrong.status, 404);

    let health = c.get("/-/healthz");
    assert_eq!(health.status, 200);
    assert_eq!(health.header("x-content-type-options"), Some("nosniff"));
    assert!(
        health
            .header("content-security-policy")
            .unwrap()
            .contains("default-src 'none'")
    );
}

#[test]
fn conf_sec_04_api_errors_are_problem_json_with_request_id() {
    let s = TestServer::start_with(&[]);
    let mut c = s.client();
    let r = c.api("GET", "/api/v1/no-such-endpoint", None, None);
    assert_eq!(r.status, 404);
    assert_eq!(r.header("content-type"), Some("application/problem+json"));
    let j = r.json();
    assert_eq!(j["status"], 404);
    assert_eq!(j["code"], "not_found");
    assert_eq!(j["request_id"].as_str(), r.header("x-request-id"));
}

fn wait_ready(s: &TestServer) -> u16 {
    let mut c = s.client();
    let mut last = 0;
    for _ in 0..50 {
        last = c.get("/-/readyz").status;
        if last == 200 {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    last
}

#[test]
fn readyz_fails_when_the_schema_does_not_match() {
    let s = TestServer::start_with(&[]);
    assert_eq!(wait_ready(&s), 200);
    s.db.execute("UPDATE schema_info SET version = version + 100");
    let r = s.client().get("/-/readyz");
    assert_eq!(r.status, 503);
    assert!(!r.body.contains("schema"), "no internals in the response");
}

#[test]
fn readyz_fails_when_the_database_is_down() {
    let s = TestServer::start_with(&[]);
    assert_eq!(wait_ready(&s), 200);
    // Refuse new connections and drop the server's existing ones.
    let name = s.db.name().to_owned();
    s.db.execute_admin(&format!(
        "ALTER DATABASE {name} ALLOW_CONNECTIONS false; \
         SELECT pg_terminate_backend(pid) FROM pg_stat_activity \
         WHERE datname = '{name}';"
    ));
    assert_eq!(s.client().get("/-/readyz").status, 503);
    assert_eq!(s.client().get("/-/healthz").status, 200);
}

#[test]
fn htmx_requests_get_fragments_with_vary() {
    let s = TestServer::start_with(&[]);
    let mut c = s.client();
    let full = c.get("/-/design?count=3");
    assert!(full.body.contains("<html"));
    assert_eq!(full.header("vary"), Some("HX-Request"));
    let frag = c.get_with(
        "/-/design?count=3",
        &[("hx-request", "true"), ("hx-request-type", "partial")],
    );
    assert!(!frag.body.contains("<html"));
    assert!(frag.body.contains("id=\"demo-region\""));
    assert!(frag.body.contains("href=\"/-/design?count=4\""));
    assert_eq!(frag.header("vary"), Some("HX-Request"));
    // The fragment is stamped with its own response's nonce.
    let n = nonce_of(&frag);
    assert!(frag.body.contains(&format!("hx-nonce=\"{n}\"")));
}
