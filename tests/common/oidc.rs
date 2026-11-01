// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! A mock OpenID Connect provider for tests: discovery, an ES256 key set,
//! and a token endpoint that returns whatever ID token the test prepared.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64;
use p256::ecdsa::signature::Signer;
use p256::ecdsa::{Signature, SigningKey};
use serde_json::{Value, json};

#[derive(Default)]
struct Shared {
    /// The next token endpoint answer.
    id_token: Option<String>,
    /// Request lines received, for assertions.
    requests: Vec<String>,
}

pub struct MockProvider {
    pub issuer: String,
    key: SigningKey,
    shared: Arc<Mutex<Shared>>,
}

fn respond(stream: &mut std::net::TcpStream, status: &str, body: &str) {
    let _ = write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
}

impl MockProvider {
    #[must_use]
    pub fn start() -> Self {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let issuer = format!("http://127.0.0.1:{}", l.local_addr().unwrap().port());
        let key = SigningKey::from_slice(&[9u8; 32]).unwrap();
        let shared = Arc::new(Mutex::new(Shared::default()));
        let point = key.verifying_key().to_sec1_point(false);
        let jwk = json!({
            "kty": "EC", "crv": "P-256", "kid": "test-key", "alg": "ES256", "use": "sig",
            "x": B64.encode(point.x().unwrap()), "y": B64.encode(point.y().unwrap()),
        });
        let discovery = json!({
            "issuer": issuer,
            "authorization_endpoint": format!("{issuer}/authorize"),
            "token_endpoint": format!("{issuer}/token"),
            "jwks_uri": format!("{issuer}/jwks"),
        });
        let s = Arc::clone(&shared);
        std::thread::spawn(move || {
            for stream in l.incoming().flatten() {
                let mut stream = stream;
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                if reader.read_line(&mut line).is_err() {
                    continue;
                }
                let mut len = 0usize;
                loop {
                    let mut h = String::new();
                    if reader.read_line(&mut h).is_err() || h.trim().is_empty() {
                        break;
                    }
                    if let Some(v) = h.to_ascii_lowercase().strip_prefix("content-length:") {
                        len = v.trim().parse().unwrap_or(0);
                    }
                }
                let mut body = vec![0u8; len];
                let _ = reader.read_exact(&mut body);
                let path = line.split_whitespace().nth(1).unwrap_or("").to_owned();
                s.lock().unwrap().requests.push(format!(
                    "{} {}",
                    line.trim(),
                    String::from_utf8_lossy(&body)
                ));
                if path.starts_with("/.well-known/openid-configuration") {
                    respond(&mut stream, "200 OK", &discovery.to_string());
                } else if path.starts_with("/jwks") {
                    respond(&mut stream, "200 OK", &json!({"keys": [jwk]}).to_string());
                } else if path.starts_with("/token") {
                    match s.lock().unwrap().id_token.take() {
                        Some(t) => respond(
                            &mut stream,
                            "200 OK",
                            &json!({"access_token": "x", "token_type": "Bearer", "id_token": t})
                                .to_string(),
                        ),
                        None => respond(
                            &mut stream,
                            "400 Bad Request",
                            r#"{"error":"invalid_grant"}"#,
                        ),
                    }
                } else {
                    respond(&mut stream, "404 Not Found", "{}");
                }
            }
        });
        Self {
            issuer,
            key,
            shared,
        }
    }

    /// The `[[auth.oidc]]` block for this provider.
    #[must_use]
    pub fn config(&self, secret_file: &std::path::Path, extra: &str) -> String {
        format!(
            "[[auth.oidc]]\nkey = \"mock\"\ndisplay_name = \"Mock ID\"\nissuer = \"{}\"\nclient_id = \"slath\"\nclient_secret_file = \"{}\"\n{extra}\n",
            self.issuer,
            secret_file.display()
        )
    }

    /// Signs claims with the provider's key (or another one).
    #[must_use]
    pub fn sign(&self, header: &Value, claims: &Value, wrong_key: bool) -> String {
        let h = B64.encode(header.to_string());
        let p = B64.encode(claims.to_string());
        let signed = format!("{h}.{p}");
        let key = if wrong_key {
            SigningKey::from_slice(&[3u8; 32]).unwrap()
        } else {
            self.key.clone()
        };
        let sig: Signature = key.sign(signed.as_bytes());
        format!("{signed}.{}", B64.encode(sig.to_bytes()))
    }

    /// Good claims for `sub` with `nonce`.
    #[must_use]
    pub fn claims(&self, sub: &str, nonce: &str) -> Value {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        json!({
            "iss": self.issuer, "sub": sub, "aud": "slath", "nonce": nonce,
            "iat": now, "exp": now + 300,
        })
    }

    /// Sets the ID token the next token request gets.
    pub fn next_token(&self, token: String) {
        self.shared.lock().unwrap().id_token = Some(token);
    }

    #[must_use]
    pub fn requests(&self) -> Vec<String> {
        self.shared.lock().unwrap().requests.clone()
    }
}

/// The `state` and `nonce` of the provider URL a sign-in started with.
#[must_use]
pub fn state_and_nonce(location: &str) -> (String, String) {
    let query = location.split_once('?').map_or("", |(_, q)| q);
    let get = |k: &str| {
        query
            .split('&')
            .find_map(|kv| kv.strip_prefix(&format!("{k}=")))
            .map(|v| v.replace("%2D", "-").replace("%5F", "_"))
            .unwrap_or_default()
    };
    (get("state"), get("nonce"))
}
