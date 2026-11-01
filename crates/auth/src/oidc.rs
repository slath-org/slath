// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! OpenID Connect relying-party primitives: discovery documents, JSON Web
//! Key Sets, PKCE, and ID token validation. No IO.
//!
//! An ID token is accepted only if its signature verifies with a key from
//! the provider's key set under an allowed algorithm (RS256, ES256, EdDSA;
//! never `none` or HMAC), `iss` equals the configured issuer exactly,
//! `aud` contains the client ID, `azp` (if present) is the client ID,
//! `exp` and `iat` are within 60 seconds of now, and `nonce` matches the
//! one sent with the request.

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use ring::signature;
use serde::Deserialize;
use sha2::{Digest, Sha256};

/// Allowed clock skew in seconds.
pub const SKEW: i64 = 60;

/// The parts of a discovery document Slath uses.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct Discovery {
    pub issuer: String,
    pub authorization_endpoint: String,
    pub token_endpoint: String,
    pub jwks_uri: String,
    #[serde(default)]
    pub userinfo_endpoint: Option<String>,
}

/// One key of a JSON Web Key Set.
#[derive(Debug, Clone, Deserialize)]
pub struct Jwk {
    pub kty: String,
    #[serde(default)]
    pub kid: Option<String>,
    #[serde(default)]
    pub alg: Option<String>,
    #[serde(default, rename = "use")]
    pub use_: Option<String>,
    #[serde(default)]
    pub n: Option<String>,
    #[serde(default)]
    pub e: Option<String>,
    #[serde(default)]
    pub crv: Option<String>,
    #[serde(default)]
    pub x: Option<String>,
    #[serde(default)]
    pub y: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Jwks {
    pub keys: Vec<Jwk>,
}

/// Why an ID token was refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TokenError {
    #[error("malformed token")]
    Malformed,
    #[error("algorithm not allowed")]
    Algorithm,
    #[error("no matching key")]
    UnknownKey,
    #[error("bad signature")]
    Signature,
    #[error("wrong issuer")]
    Issuer,
    #[error("wrong audience")]
    Audience,
    #[error("expired or not yet valid")]
    Expired,
    #[error("nonce mismatch")]
    Nonce,
    #[error("missing subject")]
    Subject,
}

/// The claims Slath uses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Claims {
    pub issuer: String,
    pub subject: String,
    pub email: Option<String>,
    pub email_verified: bool,
    pub preferred_username: Option<String>,
    pub name: Option<String>,
}

#[derive(Deserialize)]
struct Header {
    alg: String,
    #[serde(default)]
    kid: Option<String>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum Aud {
    One(String),
    Many(Vec<String>),
}

#[derive(Deserialize)]
struct RawClaims {
    iss: String,
    #[serde(default)]
    sub: String,
    aud: Aud,
    #[serde(default)]
    azp: Option<String>,
    exp: i64,
    iat: i64,
    #[serde(default)]
    nonce: Option<String>,
    #[serde(default)]
    email: Option<String>,
    #[serde(default)]
    email_verified: Option<serde_json::Value>,
    #[serde(default)]
    preferred_username: Option<String>,
    #[serde(default)]
    name: Option<String>,
}

fn b64(s: &str) -> Result<Vec<u8>, TokenError> {
    URL_SAFE_NO_PAD
        .decode(s.trim_end_matches('='))
        .map_err(|_| TokenError::Malformed)
}

/// Checks the signature of `signed` (`header.payload`) with `key`.
fn verify(alg: &str, key: &Jwk, signed: &[u8], sig: &[u8]) -> Result<(), TokenError> {
    if key.alg.as_deref().is_some_and(|a| a != alg)
        || key.use_.as_deref().is_some_and(|u| u != "sig")
    {
        return Err(TokenError::UnknownKey);
    }
    let field = |v: &Option<String>| v.as_deref().ok_or(TokenError::UnknownKey).and_then(b64);
    match alg {
        "RS256" => {
            if key.kty != "RSA" {
                return Err(TokenError::UnknownKey);
            }
            let pk = signature::RsaPublicKeyComponents {
                n: field(&key.n)?,
                e: field(&key.e)?,
            };
            pk.verify(&signature::RSA_PKCS1_2048_8192_SHA256, signed, sig)
                .map_err(|_| TokenError::Signature)
        }
        "ES256" => {
            if key.kty != "EC" || key.crv.as_deref() != Some("P-256") {
                return Err(TokenError::UnknownKey);
            }
            let (x, y) = (field(&key.x)?, field(&key.y)?);
            if x.len() != 32 || y.len() != 32 {
                return Err(TokenError::UnknownKey);
            }
            let mut point = vec![4u8];
            point.extend_from_slice(&x);
            point.extend_from_slice(&y);
            signature::UnparsedPublicKey::new(&signature::ECDSA_P256_SHA256_FIXED, point)
                .verify(signed, sig)
                .map_err(|_| TokenError::Signature)
        }
        "EdDSA" => {
            if key.kty != "OKP" || key.crv.as_deref() != Some("Ed25519") {
                return Err(TokenError::UnknownKey);
            }
            signature::UnparsedPublicKey::new(&signature::ED25519, field(&key.x)?)
                .verify(signed, sig)
                .map_err(|_| TokenError::Signature)
        }
        _ => Err(TokenError::Algorithm),
    }
}

/// The key ID of a token's header, to pick (or refresh) the key set.
pub fn key_id(token: &str) -> Result<Option<String>, TokenError> {
    let head = token.split('.').next().ok_or(TokenError::Malformed)?;
    let h: Header = serde_json::from_slice(&b64(head)?).map_err(|_| TokenError::Malformed)?;
    Ok(h.kid)
}

/// What a token must match.
#[derive(Debug, Clone)]
pub struct Expect<'a> {
    pub issuer: &'a str,
    pub client_id: &'a str,
    pub nonce: &'a str,
    /// Unix time.
    pub now: i64,
}

/// Validates an ID token and returns its claims.
pub fn validate(token: &str, keys: &Jwks, expect: &Expect<'_>) -> Result<Claims, TokenError> {
    let mut parts = token.split('.');
    let (Some(h), Some(p), Some(s), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Err(TokenError::Malformed);
    };
    let header: Header = serde_json::from_slice(&b64(h)?).map_err(|_| TokenError::Malformed)?;
    if !matches!(header.alg.as_str(), "RS256" | "ES256" | "EdDSA") {
        return Err(TokenError::Algorithm);
    }
    let sig = b64(s)?;
    let signed = &token.as_bytes()[..h.len() + 1 + p.len()];
    let candidates: Vec<&Jwk> = keys
        .keys
        .iter()
        .filter(|k| match (&header.kid, &k.kid) {
            (Some(want), Some(have)) => want == have,
            (Some(_), None) => false,
            (None, _) => true,
        })
        .collect();
    if candidates.is_empty() {
        return Err(TokenError::UnknownKey);
    }
    let mut last = TokenError::UnknownKey;
    let mut ok = false;
    for k in candidates {
        match verify(&header.alg, k, signed, &sig) {
            Ok(()) => {
                ok = true;
                break;
            }
            Err(e) => last = e,
        }
    }
    if !ok {
        return Err(last);
    }
    let c: RawClaims = serde_json::from_slice(&b64(p)?).map_err(|_| TokenError::Malformed)?;
    if c.iss != expect.issuer {
        return Err(TokenError::Issuer);
    }
    let auds = match &c.aud {
        Aud::One(a) => vec![a.as_str()],
        Aud::Many(v) => v.iter().map(String::as_str).collect(),
    };
    if !auds.contains(&expect.client_id) {
        return Err(TokenError::Audience);
    }
    if c.azp.as_deref().is_some_and(|a| a != expect.client_id) {
        return Err(TokenError::Audience);
    }
    // Several audiences need azp naming us.
    if auds.len() > 1 && c.azp.is_none() {
        return Err(TokenError::Audience);
    }
    if c.exp + SKEW < expect.now || c.iat - SKEW > expect.now {
        return Err(TokenError::Expired);
    }
    if c.nonce.as_deref() != Some(expect.nonce) {
        return Err(TokenError::Nonce);
    }
    if c.sub.is_empty() || c.sub.len() > 255 {
        return Err(TokenError::Subject);
    }
    let email_verified = match c.email_verified {
        Some(serde_json::Value::Bool(b)) => b,
        Some(serde_json::Value::String(s)) => s == "true",
        _ => false,
    };
    Ok(Claims {
        issuer: c.iss,
        subject: c.sub,
        email: c.email,
        email_verified,
        preferred_username: c.preferred_username,
        name: c.name,
    })
}

/// The PKCE S256 challenge of a verifier.
#[must_use]
pub fn pkce_challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ring::rand::SystemRandom;
    use ring::signature::{EcdsaKeyPair, KeyPair};

    fn es256() -> (EcdsaKeyPair, Jwk) {
        let rng = SystemRandom::new();
        let pkcs8 = EcdsaKeyPair::generate_pkcs8(&signature::ECDSA_P256_SHA256_FIXED_SIGNING, &rng)
            .unwrap();
        let kp = EcdsaKeyPair::from_pkcs8(
            &signature::ECDSA_P256_SHA256_FIXED_SIGNING,
            pkcs8.as_ref(),
            &rng,
        )
        .unwrap();
        let pubk = kp.public_key().as_ref();
        let jwk = Jwk {
            kty: "EC".into(),
            kid: Some("k1".into()),
            alg: Some("ES256".into()),
            use_: Some("sig".into()),
            n: None,
            e: None,
            crv: Some("P-256".into()),
            x: Some(URL_SAFE_NO_PAD.encode(&pubk[1..33])),
            y: Some(URL_SAFE_NO_PAD.encode(&pubk[33..65])),
        };
        (kp, jwk)
    }

    fn sign(kp: &EcdsaKeyPair, header: &serde_json::Value, claims: &serde_json::Value) -> String {
        let h = URL_SAFE_NO_PAD.encode(header.to_string());
        let p = URL_SAFE_NO_PAD.encode(claims.to_string());
        let signed = format!("{h}.{p}");
        let sig = kp.sign(&SystemRandom::new(), signed.as_bytes()).unwrap();
        format!("{signed}.{}", URL_SAFE_NO_PAD.encode(sig.as_ref()))
    }

    fn claims(now: i64) -> serde_json::Value {
        serde_json::json!({
            "iss": "https://id.example.org", "sub": "u-1", "aud": "slath",
            "exp": now + 300, "iat": now, "nonce": "n0nce",
            "email": "a@example.org", "email_verified": true, "preferred_username": "ann",
        })
    }

    #[test]
    fn tokens_are_validated() {
        let (kp, jwk) = es256();
        let keys = Jwks { keys: vec![jwk] };
        let now = 1_800_000_000;
        let expect = Expect {
            issuer: "https://id.example.org",
            client_id: "slath",
            nonce: "n0nce",
            now,
        };
        let header = serde_json::json!({"alg": "ES256", "kid": "k1"});
        let good = sign(&kp, &header, &claims(now));
        let c = validate(&good, &keys, &expect).unwrap();
        assert_eq!(c.subject, "u-1");
        assert!(c.email_verified);

        let with = |f: &dyn Fn(&mut serde_json::Value)| {
            let mut c = claims(now);
            f(&mut c);
            validate(&sign(&kp, &header, &c), &keys, &expect)
        };
        assert_eq!(
            with(&|c| c["iss"] = "https://evil.example".into()),
            Err(TokenError::Issuer)
        );
        assert_eq!(
            with(&|c| c["iss"] = "https://id.example.org/".into()),
            Err(TokenError::Issuer)
        );
        assert_eq!(
            with(&|c| c["aud"] = "other".into()),
            Err(TokenError::Audience)
        );
        assert_eq!(
            with(&|c| c["aud"] = serde_json::json!(["slath", "other"])),
            Err(TokenError::Audience)
        );
        assert!(
            with(&|c| {
                c["aud"] = serde_json::json!(["slath", "other"]);
                c["azp"] = "slath".into();
            })
            .is_ok()
        );
        assert_eq!(
            with(&|c| c["azp"] = "other".into()),
            Err(TokenError::Audience)
        );
        assert_eq!(
            with(&|c| c["exp"] = (now - 61).into()),
            Err(TokenError::Expired)
        );
        assert_eq!(
            with(&|c| c["iat"] = (now + 120).into()),
            Err(TokenError::Expired)
        );
        assert!(
            with(&|c| c["exp"] = (now - 30).into()).is_ok(),
            "within the skew"
        );
        assert_eq!(
            with(&|c| c["nonce"] = "other".into()),
            Err(TokenError::Nonce)
        );
        assert_eq!(
            with(&|c| {
                c.as_object_mut().unwrap().remove("nonce");
            }),
            Err(TokenError::Nonce)
        );

        // alg=none, HMAC, and tampered payloads.
        let parts: Vec<&str> = good.split('.').collect();
        let none_header = URL_SAFE_NO_PAD.encode(r#"{"alg":"none"}"#);
        assert_eq!(
            validate(&format!("{none_header}.{}.", parts[1]), &keys, &expect),
            Err(TokenError::Algorithm)
        );
        let hs = URL_SAFE_NO_PAD.encode(r#"{"alg":"HS256","kid":"k1"}"#);
        assert_eq!(
            validate(&format!("{hs}.{}.{}", parts[1], parts[2]), &keys, &expect),
            Err(TokenError::Algorithm)
        );
        let mut evil = claims(now);
        evil["sub"] = "admin".into();
        let tampered = format!(
            "{}.{}.{}",
            parts[0],
            URL_SAFE_NO_PAD.encode(evil.to_string()),
            parts[2]
        );
        assert_eq!(
            validate(&tampered, &keys, &expect),
            Err(TokenError::Signature)
        );
        // Another key, an unknown kid.
        let (other, _) = es256();
        assert_eq!(
            validate(&sign(&other, &header, &claims(now)), &keys, &expect),
            Err(TokenError::Signature)
        );
        let unknown = serde_json::json!({"alg": "ES256", "kid": "nope"});
        assert_eq!(
            validate(&sign(&kp, &unknown, &claims(now)), &keys, &expect),
            Err(TokenError::UnknownKey)
        );
        assert_eq!(validate("a.b", &keys, &expect), Err(TokenError::Malformed));
    }

    #[test]
    fn pkce_is_base64url_sha256() {
        assert_eq!(
            pkce_challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gXFrxMoAM"),
            // Computed independently (Python hashlib + urlsafe_b64encode).
            "Ahh6orzUjytys0KcifeONZb6vjFUCcuuuTdlZxlDVCw"
        );
    }
}
