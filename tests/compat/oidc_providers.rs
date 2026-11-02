// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Signing in with real OpenID providers (AUTH-01): Dex and Keycloak in
//! containers, a person signing in through the provider's own login form,
//! and Slath creating and signing in the account. These pull and start
//! container images, so they are ignored by default; the nightly workflow
//! runs them with `--run-ignored only`.

use std::process::Command;
use std::time::{Duration, Instant};

use slath_tests::http::{Client, Resp, submit};
use slath_tests::server::TestServer;

const SECRET: &str = "s3cret-client";
/// Dex's documented example hash of "password".
const DEX_PASSWORD_HASH: &str = "$2a$10$2b2cU8CPhOTaGrs1HRQuAueS7JTT5ZHsHSzYiFPm1leZck7Mc8T4W";
const DEX_IMAGE: &str = "ghcr.io/dexidp/dex:v2.44.0";
const KEYCLOAK_IMAGE: &str = "quay.io/keycloak/keycloak:26.4.2";

fn engine() -> String {
    std::env::var("SLATH_CONTAINER_ENGINE").unwrap_or_else(|_| "docker".to_owned())
}

/// A free TCP port on 127.0.0.1.
fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// A container that is removed when dropped.
struct Container(String);

impl Drop for Container {
    fn drop(&mut self) {
        let _ = Command::new(engine()).args(["rm", "-f", &self.0]).output();
    }
}

fn start(args: &[&str]) -> Container {
    let out = Command::new(engine())
        .args(["run", "-d", "--network", "host"])
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "starting the container failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    Container(String::from_utf8_lossy(&out.stdout).trim().to_owned())
}

fn wait_for(url: &str, secs: u64) {
    let start = Instant::now();
    let mut c = Client::new(url);
    loop {
        let ok =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| c.get(url).status == 200))
                .unwrap_or(false);
        if ok {
            return;
        }
        assert!(
            start.elapsed() < Duration::from_secs(secs),
            "{url} didn't come up"
        );
        std::thread::sleep(Duration::from_millis(500));
    }
}

/// The `action` and hidden fields of the first form with a password field.
fn login_form(html: &str) -> Option<(String, Vec<(String, String)>)> {
    let form_start = html.find("<form")?;
    let rest = &html[form_start..];
    let form_end = rest.find("</form>")?;
    let form = &rest[..form_end];
    if !form.contains("type=\"password\"") {
        return None;
    }
    let attr = |tag: &str, name: &str| -> Option<String> {
        let key = format!("{name}=\"");
        let i = tag.find(&key)? + key.len();
        Some(tag[i..].split('"').next()?.replace("&amp;", "&"))
    };
    let action = attr(&form[..form.find('>')?], "action")?;
    let mut hidden = Vec::new();
    for input in form.split("<input").skip(1) {
        let tag = &input[..input.find('>').unwrap_or(input.len())];
        if tag.contains("type=\"hidden\"")
            && let (Some(n), Some(v)) = (attr(tag, "name"), attr(tag, "value"))
        {
            hidden.push((n, v));
        }
    }
    Some((action, hidden))
}

fn absolute(base: &str, location: &str) -> String {
    if location.starts_with("http") {
        location.to_owned()
    } else {
        let origin: String = base.splitn(4, '/').take(3).collect::<Vec<_>>().join("/");
        format!("{origin}{location}")
    }
}

/// Signs in at the provider like a browser: follows redirects, fills in
/// the login form, and returns the Slath callback URL it is sent back to.
fn sign_in_at_provider(
    slath: &str,
    start: &str,
    user_field: &str,
    user: &str,
    password: &str,
) -> String {
    let mut idp = Client::new(start);
    idp.default_headers.clear();
    let mut url = start.to_owned();
    let mut posted = false;
    for _ in 0..20 {
        let r = idp.get(&url);
        let r = match (r.status, login_form(&r.body)) {
            (200, Some((action, hidden))) if !posted => {
                posted = true;
                let mut fields: Vec<(&str, &str)> = hidden
                    .iter()
                    .map(|(k, v)| (k.as_str(), v.as_str()))
                    .collect();
                fields.push((user_field, user));
                fields.push(("password", password));
                idp.post_form(&absolute(&url, &action), &fields)
            }
            _ => r,
        };
        let Some(location) = r.header("location").map(str::to_owned) else {
            panic!(
                "provider answered {} at {url}: {}",
                r.status,
                &r.body[..r.body.len().min(2000)]
            );
        };
        let next = absolute(&url, &location);
        if next.starts_with(slath) {
            return next;
        }
        url = next;
    }
    panic!("too many redirects at the provider");
}

/// Signs in through the provider, confirms the new account, and checks the
/// session.
fn full_flow(s: &TestServer, key: &str, user_field: &str, user: &str, password: &str) -> Client {
    let mut c = s.client();
    let r = c.get(&format!("/-/login/oidc/{key}?next=/"));
    assert_eq!(r.status, 303, "{}", r.body);
    let authorize = r.header("location").unwrap().to_owned();
    assert!(authorize.contains("code_challenge_method=S256"));
    let callback = sign_in_at_provider(&s.base(), &authorize, user_field, user, password);
    let path = &callback[s.base().len()..];
    let r: Resp = c.get(path);
    assert_eq!(
        r.header("location"),
        Some("/-/login/oidc/register"),
        "{} {}",
        r.status,
        r.body
    );
    let r = submit(
        &mut c,
        "/-/login/oidc/register",
        "/-/login/oidc/register",
        &[("username", "viaoidc")],
    );
    assert_eq!(r.status, 303, "{}", r.body);
    assert_eq!(c.get("/-/settings/security").status, 200, "signed in");
    assert_eq!(
        s.db.query_one("SELECT count(*)::text FROM oidc_identities"),
        "1"
    );
    // Signing out and in again uses the linked identity.
    let mut again = s.client();
    let r = again.get(&format!("/-/login/oidc/{key}?next=/viaoidc"));
    let callback = sign_in_at_provider(
        &s.base(),
        r.header("location").unwrap(),
        user_field,
        user,
        password,
    );
    let r = again.get(&callback[s.base().len()..]);
    assert_eq!(r.header("location"), Some("/viaoidc"), "{}", r.body);
    c
}

#[test]
#[ignore = "starts a Dex container; run by the nightly OIDC workflow"]
fn conf_auth_01_dex_sign_in() {
    let port = free_port();
    let issuer = format!("http://127.0.0.1:{port}/dex");
    let dir = tempfile::tempdir().unwrap();
    let secret = dir.path().join("secret");
    std::fs::write(&secret, SECRET).unwrap();
    let s = TestServer::start_full(
        &[],
        &format!(
            "[[auth.oidc]]\nkey = \"dex\"\ndisplay_name = \"Dex\"\nissuer = \"{issuer}\"\nclient_id = \"slath\"\nclient_secret_file = \"{}\"\nallow_registration = true\n",
            secret.display()
        ),
    );
    let config = format!(
        "issuer: {issuer}\nstorage:\n  type: memory\nweb:\n  http: 127.0.0.1:{port}\noauth2:\n  skipApprovalScreen: true\nenablePasswordDB: true\nstaticClients:\n- id: slath\n  secret: {SECRET}\n  name: Slath\n  redirectURIs:\n  - '{}/-/login/oidc/dex/callback'\nstaticPasswords:\n- email: dex@example.org\n  hash: '{DEX_PASSWORD_HASH}'\n  username: dexuser\n  userID: 08a8684b-db88-4b73-90a9-3cd1661f5466\n",
        s.base()
    );
    let file = dir.path().join("dex.yaml");
    std::fs::write(&file, config).unwrap();
    let mount = format!("{}:/etc/dex/config.yaml:ro", file.display());
    let _dex = start(&[
        "-v",
        &mount,
        DEX_IMAGE,
        "dex",
        "serve",
        "/etc/dex/config.yaml",
    ]);
    wait_for(&format!("{issuer}/.well-known/openid-configuration"), 60);
    full_flow(&s, "dex", "login", "dex@example.org", "password");
}

#[test]
#[ignore = "starts a Keycloak container; run by the nightly OIDC workflow"]
fn conf_auth_01_keycloak_sign_in() {
    let port = free_port();
    let issuer = format!("http://127.0.0.1:{port}/realms/slath");
    let dir = tempfile::tempdir().unwrap();
    let secret = dir.path().join("secret");
    std::fs::write(&secret, SECRET).unwrap();
    let s = TestServer::start_full(
        &[],
        &format!(
            "[[auth.oidc]]\nkey = \"kc\"\ndisplay_name = \"Keycloak\"\nissuer = \"{issuer}\"\nclient_id = \"slath\"\nclient_secret_file = \"{}\"\nallow_registration = true\n",
            secret.display()
        ),
    );
    let realm = serde_json::json!({
        "realm": "slath",
        "enabled": true,
        "users": [{
            "username": "kcuser", "enabled": true, "email": "kc@example.org", "emailVerified": true,
            "firstName": "Kay", "lastName": "Cee",
            "credentials": [{"type": "password", "value": "password", "temporary": false}],
        }],
        "clients": [{
            "clientId": "slath", "secret": SECRET, "publicClient": false, "enabled": true,
            "standardFlowEnabled": true, "directAccessGrantsEnabled": false,
            "redirectUris": [format!("{}/-/login/oidc/kc/callback", s.base())],
            "attributes": {"pkce.code.challenge.method": "S256"},
        }],
    });
    let realm_dir = dir.path().join("import");
    std::fs::create_dir_all(&realm_dir).unwrap();
    std::fs::write(realm_dir.join("slath.json"), realm.to_string()).unwrap();
    let mount = format!("{}:/opt/keycloak/data/import:ro", realm_dir.display());
    let http_port = format!("--http-port={port}");
    let _kc = start(&[
        "-e",
        "KC_BOOTSTRAP_ADMIN_USERNAME=admin",
        "-e",
        "KC_BOOTSTRAP_ADMIN_PASSWORD=admin-password",
        "-v",
        &mount,
        KEYCLOAK_IMAGE,
        "start-dev",
        "--import-realm",
        &http_port,
    ]);
    wait_for(&format!("{issuer}/.well-known/openid-configuration"), 180);
    full_flow(&s, "kc", "username", "kcuser", "password");
}
