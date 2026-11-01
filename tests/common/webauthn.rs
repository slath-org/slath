// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! A software passkey (ES256, "none" attestation) for tests: answers the
//! server's WebAuthn options like a browser and authenticator would, with
//! knobs to forge the origin, the RP ID, and the flags.

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64;
use p256::ecdsa::signature::Signer;
use p256::ecdsa::{Signature, SigningKey};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

pub struct SoftPasskey {
    key: SigningKey,
    pub credential_id: Vec<u8>,
    pub user_handle: Vec<u8>,
    counter: u32,
    /// Origin put into clientDataJSON.
    pub origin: String,
    /// RP ID whose hash goes into authenticatorData.
    pub rp_id: String,
    /// Set the user-verified flag.
    pub user_verified: bool,
}

/// Minimal CBOR encoding of the few shapes WebAuthn needs.
enum Cbor<'a> {
    Int(i64),
    Bytes(&'a [u8]),
    Text(&'a str),
    Map(Vec<(Cbor<'a>, Cbor<'a>)>),
}

fn head(out: &mut Vec<u8>, major: u8, n: u64) {
    let m = major << 5;
    match n {
        0..=23 => out.push(m | n as u8),
        24..=0xff => out.extend([m | 24, n as u8]),
        0x100..=0xffff => {
            out.push(m | 25);
            out.extend((n as u16).to_be_bytes());
        }
        _ => {
            out.push(m | 26);
            out.extend((n as u32).to_be_bytes());
        }
    }
}

fn encode(c: &Cbor<'_>, out: &mut Vec<u8>) {
    match c {
        Cbor::Int(i) if *i >= 0 => head(out, 0, *i as u64),
        Cbor::Int(i) => head(out, 1, (-1 - *i) as u64),
        Cbor::Bytes(b) => {
            head(out, 2, b.len() as u64);
            out.extend_from_slice(b);
        }
        Cbor::Text(t) => {
            head(out, 3, t.len() as u64);
            out.extend_from_slice(t.as_bytes());
        }
        Cbor::Map(m) => {
            head(out, 5, m.len() as u64);
            for (k, v) in m {
                encode(k, out);
                encode(v, out);
            }
        }
    }
}

fn b64(s: &str) -> Vec<u8> {
    B64.decode(s).unwrap()
}

impl SoftPasskey {
    /// A new passkey for `origin` (its host is the RP ID).
    #[must_use]
    pub fn new(origin: &str, seed: u8) -> Self {
        let mut secret = [7u8; 32];
        secret[0] = seed.max(1);
        let rp_id = origin
            .split("://")
            .nth(1)
            .unwrap()
            .split(':')
            .next()
            .unwrap()
            .to_owned();
        Self {
            key: SigningKey::from_slice(&secret).unwrap(),
            credential_id: (0..16).map(|i| i ^ seed).collect(),
            user_handle: Vec::new(),
            counter: 0,
            origin: origin.to_owned(),
            rp_id,
            user_verified: true,
        }
    }

    fn client_data(&self, kind: &str, challenge: &str) -> Vec<u8> {
        serde_json::to_vec(&json!({
            "type": kind,
            "challenge": challenge,
            "origin": self.origin,
            "crossOrigin": false,
        }))
        .unwrap()
    }

    fn auth_data(&mut self, attested: bool) -> Vec<u8> {
        self.counter += 1;
        let mut d = Sha256::digest(self.rp_id.as_bytes()).to_vec();
        let mut flags = 0x01; // user present
        if self.user_verified {
            flags |= 0x04;
        }
        if attested {
            flags |= 0x40;
        }
        d.push(flags);
        d.extend(self.counter.to_be_bytes());
        if attested {
            d.extend([0u8; 16]); // AAGUID
            d.extend((self.credential_id.len() as u16).to_be_bytes());
            d.extend(&self.credential_id);
            let point = self.key.verifying_key().to_sec1_point(false);
            let cose = Cbor::Map(vec![
                (Cbor::Int(1), Cbor::Int(2)),
                (Cbor::Int(3), Cbor::Int(-7)),
                (Cbor::Int(-1), Cbor::Int(1)),
                (Cbor::Int(-2), Cbor::Bytes(point.x().unwrap())),
                (Cbor::Int(-3), Cbor::Bytes(point.y().unwrap())),
            ]);
            encode(&cose, &mut d);
        }
        d
    }

    /// Answers `navigator.credentials.create` options.
    pub fn create(&mut self, options: &Value) -> Value {
        let pk = &options["publicKey"];
        let challenge = pk["challenge"].as_str().unwrap();
        self.user_handle = b64(pk["user"]["id"].as_str().unwrap());
        let client_data = self.client_data("webauthn.create", challenge);
        let auth_data = self.auth_data(true);
        let mut att = Vec::new();
        encode(
            &Cbor::Map(vec![
                (Cbor::Text("fmt"), Cbor::Text("none")),
                (Cbor::Text("attStmt"), Cbor::Map(vec![])),
                (Cbor::Text("authData"), Cbor::Bytes(&auth_data)),
            ]),
            &mut att,
        );
        let id = B64.encode(&self.credential_id);
        json!({
            "id": id,
            "rawId": id,
            "type": "public-key",
            "response": {
                "attestationObject": B64.encode(att),
                "clientDataJSON": B64.encode(client_data),
                "transports": ["internal"],
            },
            "extensions": {},
        })
    }

    /// Answers `navigator.credentials.get` options.
    pub fn get(&mut self, options: &Value) -> Value {
        let challenge = options["publicKey"]["challenge"].as_str().unwrap();
        let client_data = self.client_data("webauthn.get", challenge);
        let auth_data = self.auth_data(false);
        let mut signed = auth_data.clone();
        signed.extend(Sha256::digest(&client_data));
        let sig: Signature = self.key.sign(&signed);
        let id = B64.encode(&self.credential_id);
        json!({
            "id": id,
            "rawId": id,
            "type": "public-key",
            "response": {
                "authenticatorData": B64.encode(auth_data),
                "clientDataJSON": B64.encode(client_data),
                "signature": B64.encode(sig.to_der().as_bytes()),
                "userHandle": B64.encode(&self.user_handle),
            },
            "extensions": {},
        })
    }
}
