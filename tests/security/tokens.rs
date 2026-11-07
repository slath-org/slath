// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Personal access tokens.

use slath_tests::http::{Client, submit};
use slath_tests::server::TestServer;

const PW: &str = "correct horse battery staple";

/// Creates a token through the settings page and returns it.
pub fn create_token(c: &mut Client, name: &str, scopes: &[&str], expires: &str) -> String {
    let mut fields = vec![("name", name), ("expires", expires)];
    for s in scopes {
        fields.push(("scopes", s));
    }
    let r = submit(c, "/-/settings/tokens", "/-/settings/tokens", &fields);
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.header("cache-control"), Some("no-store"));
    let start = r.body.find("value=\"slath_pat_").expect("token shown") + "value=\"".len();
    r.body[start..start + slath_core::credential::TOKEN_LEN].to_owned()
}

#[test]
fn conf_auth_05_token_is_shown_once_and_authenticates_the_api() {
    let s = TestServer::start_with(&[]);
    s.create_user("alice", PW, false);
    let mut a = s.login("alice", PW);
    let token = create_token(&mut a, "laptop", &["user:read"], "30");
    assert!(token.starts_with("slath_pat_"));
    // The list shows only the prefix.
    let list = a.get("/-/settings/tokens");
    assert!(!list.body.contains(&token));
    assert!(
        list.body
            .contains(&format!("slath_pat_{}…", &token[10..18]))
    );
    // The token works with the API.
    let mut api = s.client();
    let r = api.api("GET", "/api/v1/user", Some(&token), None);
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.json()["username"], "alice");
    // last_used_at was recorded.
    assert!(
        a.get("/-/settings/tokens").body.contains("<td><time"),
        "last used shown"
    );
}

#[test]
fn conf_auth_05_revoked_expired_and_malformed_tokens_get_401() {
    let s = TestServer::start_with(&[]);
    s.create_user("alice", PW, false);
    let mut a = s.login("alice", PW);
    let revoked = create_token(&mut a, "revoked", &["user:read"], "30");
    let expired = create_token(&mut a, "expired", &["user:read"], "7");
    s.db.execute(
        "UPDATE access_tokens SET expires_at = now() - interval '1 second' WHERE name = 'expired'",
    );
    let page = a.get("/-/settings/tokens");
    let id = page
        .body
        .split("/-/settings/tokens/")
        .find(|p| p.contains("/revoke") && page.body.contains("revoked"))
        .unwrap();
    let id = &id[..36];
    let r = submit(
        &mut a,
        "/-/settings/tokens",
        &format!("/-/settings/tokens/{id}/revoke"),
        &[],
    );
    assert_eq!(r.status, 303);
    let mut api = s.client();
    // Well-formed with a valid checksum, but never issued.
    let bogus = slath_auth::token::new_pat().unwrap().expose().clone();
    let mut bad_sum = bogus.clone().into_bytes();
    bad_sum[58] = if bad_sum[58] == b'0' { b'1' } else { b'0' };
    let bad_sum = String::from_utf8(bad_sum).unwrap();
    let old_format = format!("slath_pat_{}", "A".repeat(43));
    for (what, t) in [
        ("revoked", revoked.as_str()),
        ("expired", expired.as_str()),
        ("unknown", bogus.as_str()),
        ("bad checksum", bad_sum.as_str()),
        ("preview format", old_format.as_str()),
        ("malformed", "x"),
    ] {
        let r = api.api("GET", "/api/v1/user", Some(t), None);
        assert_eq!(r.status, 401, "{what}: {}", r.body);
        assert_eq!(r.json()["code"], "unauthenticated");
        assert!(
            r.header("www-authenticate")
                .is_some_and(|v| v.starts_with("Bearer")),
            "{what}"
        );
    }
}

#[test]
fn conf_rbac_02_token_scope_caps_user_endpoints() {
    let s = TestServer::start_with(&[]);
    s.create_user("alice", PW, false);
    let mut a = s.login("alice", PW);
    let repo_only = create_token(&mut a, "repo only", &["repo:read"], "30");
    let r = s
        .client()
        .api("GET", "/api/v1/user", Some(&repo_only), None);
    assert_eq!(r.status, 403, "{}", r.body);
    assert_eq!(r.json()["code"], "forbidden");
}

#[test]
fn conf_rbac_02_api_ignores_cookies() {
    let s = TestServer::start_with(&[]);
    s.create_user("alice", PW, false);
    let mut a = s.login("alice", PW);
    // The signed-in browser's cookies don't authenticate API requests.
    let r = a.request("GET", "/api/v1/user", &[], None);
    assert_eq!(r.status, 401);
}

#[test]
fn conf_rbac_02_admin_scope_is_for_site_admins_only() {
    let s = TestServer::start_with(&[]);
    s.create_user("alice", PW, false);
    let mut a = s.login("alice", PW);
    let r = submit(
        &mut a,
        "/-/settings/tokens",
        "/-/settings/tokens",
        &[("name", "x"), ("expires", "30"), ("scopes", "admin")],
    );
    assert_eq!(r.status, 422);
    assert!(
        r.body
            .contains("Only site administrators can create admin tokens.")
    );
    assert!(!r.body.contains("value=\"slath_pat_"));
}

#[test]
fn conf_auth_05_creating_tokens_needs_sudo() {
    let s = TestServer::start_with(&[]);
    s.create_user("alice", PW, false);
    let mut a = s.login("alice", PW);
    s.db.execute("UPDATE sessions SET sudo_until = NULL");
    let r = submit(
        &mut a,
        "/-/settings/tokens",
        "/-/settings/tokens",
        &[("name", "x"), ("expires", "30"), ("scopes", "repo:read")],
    );
    assert_eq!(r.status, 303);
    assert!(r.header("location").unwrap().starts_with("/-/sudo?next="));
    assert_eq!(
        s.db.query_one("SELECT count(*)::text FROM access_tokens"),
        "0"
    );
}

#[test]
fn conf_sec_04_tokens_never_appear_in_logs() {
    let s = TestServer::start_with(&[]);
    s.create_user("alice", PW, false);
    let mut a = s.login("alice", PW);
    let token = create_token(&mut a, "x", &["user:read"], "30");
    let mut api = s.client();
    api.api("GET", "/api/v1/user", Some(&token), None);
    api.api(
        "GET",
        &format!("/api/v1/user?token={token}"),
        Some("slath_pat_invalid"),
        None,
    );
    api.get(&format!("/-/no-such-page?access_token={token}"));
    let log = s.log();
    assert!(log.contains("request"), "the log has request lines");
    assert!(!log.contains("slath_pat_"), "token material in the log");
    assert!(!log.contains(&token[10..]));
}

#[test]
fn conf_rbac_02_other_users_tokens_are_not_found() {
    let s = TestServer::start_with(&[]);
    s.create_user("alice", PW, false);
    s.create_user("mallory", PW, false);
    let mut a = s.login("alice", PW);
    let token = create_token(&mut a, "alice token", &["user:read"], "30");
    let page = a.get("/-/settings/tokens");
    let id = page.body.split("/-/settings/tokens/").nth(1).unwrap()[..36].to_owned();
    let mut m = s.login("mallory", PW);
    let r = submit(
        &mut m,
        "/-/settings/tokens",
        &format!("/-/settings/tokens/{id}/revoke"),
        &[],
    );
    assert_eq!(r.status, 404);
    assert_eq!(
        s.client()
            .api("GET", "/api/v1/user", Some(&token), None)
            .status,
        200
    );
}

/// Every token kind is found exactly, and the project's own files (code,
/// docs, fixtures, test keys) contain nothing the rule would flag.
#[test]
fn conf_sec_08_slath_tokens_are_found_exactly() {
    use slath_core::credential::{CredentialKind, find_all};
    for kind in CredentialKind::ALL {
        let t = slath_auth::token::new_credential(kind).unwrap();
        for text in [
            t.expose().clone(),
            format!("export TOKEN={}\n", t.expose()),
            format!("https://git:{}@example.org/r.git", t.expose()),
            format!("{{\"token\":\"{}\"}}", t.expose()),
        ] {
            let found = find_all(text.as_bytes());
            assert_eq!(found.len(), 1, "{kind:?} in {text}");
            assert_eq!(found[0].kind, kind);
        }
        // A changed character or truncation is no longer a token.
        let mut altered = t.expose().clone().into_bytes();
        altered[30] = if altered[30] == b'q' { b'r' } else { b'q' };
        assert!(find_all(&altered).is_empty());
        assert!(find_all(&t.expose().as_bytes()[..58]).is_empty());
    }

    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    let mut stack = vec![root];
    let mut files = 0;
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).unwrap() {
            let entry = entry.unwrap();
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if [".git", "target", "node_modules", "dist", ".jj"].contains(&name.as_ref()) {
                continue;
            }
            let ty = entry.file_type().unwrap();
            if ty.is_dir() {
                stack.push(entry.path());
            } else if ty.is_file() {
                let data = std::fs::read(entry.path()).unwrap();
                files += 1;
                assert!(
                    find_all(&data).is_empty(),
                    "false positive in {}",
                    entry.path().display()
                );
            }
        }
    }
    assert!(files > 500, "corpus too small: {files}");
}
