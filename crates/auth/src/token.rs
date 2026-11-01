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

/// Prefix of personal access tokens (also used by log redaction tests).
pub const PAT_PREFIX: &str = "slath_pat_";
/// Length of the random part of a token (base62; 43 × log2(62) ≥ 256 bits).
pub const PAT_RANDOM_LEN: usize = 43;

const BASE62: &[u8] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";

/// A new personal access token, `slath_pat_` + 43 base62 characters.
pub fn new_pat() -> Result<Secret<String>, RandomError> {
    let mut out = String::with_capacity(PAT_PREFIX.len() + PAT_RANDOM_LEN);
    out.push_str(PAT_PREFIX);
    while out.len() < PAT_PREFIX.len() + PAT_RANDOM_LEN {
        for b in random_bytes::<64>()? {
            // Rejection sampling keeps every character uniform: 248 = 4 × 62.
            if b < 248 && out.len() < PAT_PREFIX.len() + PAT_RANDOM_LEN {
                out.push(BASE62[usize::from(b % 62)] as char);
            }
        }
    }
    Ok(Secret::new(out))
}

/// True if `s` has the shape of a personal access token.
#[must_use]
pub fn is_pat(s: &str) -> bool {
    s.strip_prefix(PAT_PREFIX)
        .is_some_and(|r| r.len() == PAT_RANDOM_LEN && r.bytes().all(|b| b.is_ascii_alphanumeric()))
}

/// The display prefix stored with a token: the first 8 random characters.
#[must_use]
pub fn pat_display_prefix(token: &str) -> String {
    token
        .get(PAT_PREFIX.len()..PAT_PREFIX.len() + 8)
        .unwrap_or_default()
        .to_owned()
}

#[cfg(test)]
mod pat_tests {
    use super::*;

    #[test]
    fn pats() {
        let t = new_pat().unwrap();
        assert!(is_pat(t.expose()), "{}", t.expose());
        assert_eq!(t.expose().len(), 53);
        assert_eq!(pat_display_prefix(t.expose()).len(), 8);
        assert_ne!(new_pat().unwrap().expose(), t.expose());
        assert!(!is_pat("slath_pat_short"));
        assert!(!is_pat(&format!("slath_pat_{}", "-".repeat(43))));
        assert!(!is_pat(&"a".repeat(53)));
    }
}
