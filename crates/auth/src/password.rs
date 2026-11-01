// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Password policy and Argon2id hashing.
//!
//! Hashes are PHC strings (`$argon2id$v=19$m=19456,t=2,p=1$…`), so the
//! parameters travel with each hash and old hashes are upgraded on login
//! ([`needs_rehash`]). Hashing is CPU-heavy: callers run it on a blocking
//! thread.

use argon2::password_hash::phc::PasswordHash;
use argon2::password_hash::{PasswordHasher, PasswordVerifier};
use argon2::{Algorithm, Argon2, Params, Version};
use slath_core::Secret;
use unicode_normalization::UnicodeNormalization;

/// Minimum and maximum length in bytes after NFC normalization.
pub const MIN_LEN: usize = 12;
pub const MAX_LEN: usize = 1024;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PolicyError {
    #[error("Use at least {MIN_LEN} characters.")]
    TooShort,
    #[error("Use at most {MAX_LEN} bytes.")]
    TooLong,
    #[error("The password must not be your username or email address.")]
    SameAsIdentity,
}

#[derive(Debug, thiserror::Error)]
#[error("password hashing failed")]
pub struct HashError;

fn argon2() -> Argon2<'static> {
    // The defaults are the OWASP minimum: m=19456 KiB, t=2, p=1.
    Argon2::new(Algorithm::Argon2id, Version::V0x13, Params::DEFAULT)
}

/// NFC-normalizes a password so that equivalent spellings hash the same.
#[must_use]
pub fn normalize(password: &str) -> Secret<String> {
    Secret::new(password.nfc().collect())
}

/// Checks a new password against the policy: 12–1024 bytes (NFC), and not
/// equal to the username or email (case-insensitive). No composition rules.
pub fn check_policy(
    password: &Secret<String>,
    username: &str,
    email: Option<&str>,
) -> Result<(), PolicyError> {
    let p = password.expose();
    if p.chars().count() < MIN_LEN {
        return Err(PolicyError::TooShort);
    }
    if p.len() > MAX_LEN {
        return Err(PolicyError::TooLong);
    }
    let same = |other: &str| !other.is_empty() && p.to_lowercase() == other.to_lowercase();
    if same(username) || email.is_some_and(same) {
        return Err(PolicyError::SameAsIdentity);
    }
    Ok(())
}

/// Hashes a (normalized) password into a PHC string.
pub fn hash(password: &Secret<String>) -> Result<String, HashError> {
    argon2()
        .hash_password(password.expose().as_bytes())
        .map(|h| h.to_string())
        .map_err(|_| HashError)
}

/// Verifies a password against a stored PHC string. Malformed hashes never verify.
#[must_use]
pub fn verify(password: &Secret<String>, phc: &str) -> bool {
    let Ok(parsed) = PasswordHash::new(phc) else {
        return false;
    };
    argon2()
        .verify_password(password.expose().as_bytes(), &parsed)
        .is_ok()
}

/// Spends the same effort as [`verify`] for an account that doesn't exist,
/// so response times don't reveal whether a username exists.
pub fn verify_dummy(password: &Secret<String>) {
    static DUMMY: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    if let Some(h) =
        DUMMY.get_or_init(|| hash(&Secret::new("dummy password for timing".to_owned())).ok())
    {
        let _ = verify(password, h);
    }
}

/// True if `phc` wasn't made with the current algorithm and parameters.
#[must_use]
pub fn needs_rehash(phc: &str) -> bool {
    let Ok(parsed) = PasswordHash::new(phc) else {
        return true;
    };
    let current = Params::DEFAULT;
    let param = |name: &str| parsed.params.get_decimal(name);
    parsed.algorithm.as_str() != "argon2id"
        || parsed.version != Some(0x13)
        || param("m") != Some(current.m_cost())
        || param("t") != Some(current.t_cost())
        || param("p") != Some(current.p_cost())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &str) -> Secret<String> {
        Secret::new(v.to_owned())
    }

    #[test]
    fn hash_format_and_verification() {
        let h = hash(&s("correct horse battery")).unwrap();
        assert!(h.starts_with("$argon2id$v=19$m=19456,t=2,p=1$"), "{h}");
        assert!(verify(&s("correct horse battery"), &h));
        assert!(!verify(&s("correct horse batterY"), &h));
        assert!(!verify(&s("x"), "not a hash"));
        assert!(!needs_rehash(&h));
        let weak = h.replace("m=19456", "m=4096");
        assert!(needs_rehash(&weak));
        assert!(needs_rehash(
            "$argon2i$v=19$m=19456,t=2,p=1$c2FsdHNhbHQ$aGFzaA"
        ));
    }

    #[test]
    fn policy() {
        assert_eq!(
            check_policy(&s("short"), "alice", None),
            Err(PolicyError::TooShort)
        );
        assert_eq!(
            check_policy(&s(&"x".repeat(1025)), "alice", None),
            Err(PolicyError::TooLong)
        );
        assert_eq!(
            check_policy(&s("Alice-Example1"), "alice-example1", None),
            Err(PolicyError::SameAsIdentity)
        );
        assert_eq!(
            check_policy(&s("alice@example.org"), "alice", Some("Alice@Example.org")),
            Err(PolicyError::SameAsIdentity)
        );
        assert!(check_policy(&s("twelve chars"), "alice", None).is_ok());
        // Twelve characters, even if multi-byte.
        assert!(check_policy(&s("ääääääääääää"), "alice", None).is_ok());
    }

    #[test]
    fn nfc_normalization_makes_spellings_equal() {
        let composed = normalize("caf\u{e9} au lait!!");
        let decomposed = normalize("cafe\u{301} au lait!!");
        assert_eq!(composed.expose(), decomposed.expose());
        let h = hash(&composed).unwrap();
        assert!(verify(&decomposed, &h));
    }
}
