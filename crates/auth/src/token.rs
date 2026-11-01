// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Random bearer secrets (session IDs) and their storage hashes.

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use sha2::{Digest, Sha256};
use slath_core::Secret;

#[derive(Debug, thiserror::Error)]
#[error("the operating system has no randomness source")]
pub struct RandomError;

/// `n` random bytes from the operating system.
pub fn random_bytes<const N: usize>() -> Result<[u8; N], RandomError> {
    let mut b = [0u8; N];
    getrandom::fill(&mut b).map_err(|_| RandomError)?;
    Ok(b)
}

/// A new session ID: 32 random bytes, base64url without padding.
pub fn new_session_id() -> Result<Secret<String>, RandomError> {
    Ok(Secret::new(URL_SAFE_NO_PAD.encode(random_bytes::<32>()?)))
}

/// SHA-256 of a secret, as stored in the database. Bearer secrets have 256
/// bits of entropy, so a fast hash is enough (no password stretching).
#[must_use]
pub fn storage_hash(secret: &str) -> [u8; 32] {
    Sha256::digest(secret.as_bytes()).into()
}

/// True if `s` looks like a session ID (43 base64url characters).
#[must_use]
pub fn is_session_id(s: &str) -> bool {
    s.len() == 43
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_ids() {
        let a = new_session_id().unwrap();
        let b = new_session_id().unwrap();
        assert!(is_session_id(a.expose()));
        assert_ne!(a.expose(), b.expose());
        assert_ne!(storage_hash(a.expose()), storage_hash(b.expose()));
        assert!(!is_session_id("short"));
        assert!(!is_session_id(&"a".repeat(42)));
        assert!(!is_session_id(&format!("{}=", "a".repeat(42))));
    }
}
