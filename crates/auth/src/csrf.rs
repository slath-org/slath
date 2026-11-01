// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! CSRF tokens: HMAC-SHA256 of a per-browser seed (the session ID, or a
//! random pre-session cookie before sign-in) under a key derived from the
//! master key. A token is useless without the matching cookie, and the
//! cookie alone doesn't let a cross-site page compute the token.

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use hmac::{Hmac, KeyInit, Mac};
use sha2::Sha256;
use slath_core::Secret;
use subtle::ConstantTimeEq;

/// Computes and checks CSRF tokens.
pub struct Csrf {
    key: Secret<[u8; 32]>,
}

impl std::fmt::Debug for Csrf {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Csrf(***)")
    }
}

impl Csrf {
    #[must_use]
    pub fn new(key: Secret<[u8; 32]>) -> Self {
        Self { key }
    }

    /// The token for `seed`.
    #[must_use]
    pub fn token(&self, seed: &str) -> String {
        // HMAC accepts keys of any length; 32 bytes can't fail.
        let Ok(mut mac) = Hmac::<Sha256>::new_from_slice(self.key.expose()) else {
            return String::new();
        };
        mac.update(b"slath csrf v1\0");
        mac.update(seed.as_bytes());
        URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes())
    }

    /// Constant-time check of `token` against `seed`.
    #[must_use]
    pub fn verify(&self, seed: &str, token: &str) -> bool {
        let want = self.token(seed);
        !want.is_empty() && bool::from(want.as_bytes().ct_eq(token.as_bytes()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_bind_to_seed_and_key() {
        let a = Csrf::new(Secret::new([1; 32]));
        let b = Csrf::new(Secret::new([2; 32]));
        let t = a.token("session-1");
        assert!(a.verify("session-1", &t));
        assert!(!a.verify("session-2", &t));
        assert!(!b.verify("session-1", &t));
        assert!(!a.verify("session-1", ""));
        assert!(!a.verify("session-1", &t[1..]));
    }
}
