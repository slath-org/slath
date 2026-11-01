// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Time-based one-time passwords (RFC 6238: HMAC-SHA1, 6 digits, 30 s
//! steps) and recovery codes.
//!
//! Codes from the previous and the next step are accepted (clock drift).
//! A code is accepted only for a step after the last one used, so the same
//! code can't be replayed, not even within its step.

use ring::hmac;
use slath_core::Secret;

/// Seed length in bytes (160 bits, as RFC 4226 recommends).
pub const SECRET_LEN: usize = 20;
const STEP_SECS: i64 = 30;
const DIGITS: u32 = 6;

/// RFC 4648 base32 without padding (what authenticator apps expect).
#[must_use]
pub fn base32(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 32] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";
    let mut out = String::with_capacity(bytes.len().div_ceil(5) * 8);
    let (mut buf, mut bits) = (0u32, 0u32);
    for &b in bytes {
        buf = (buf << 8) | u32::from(b);
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            out.push(char::from(ALPHABET[((buf >> bits) & 31) as usize]));
        }
    }
    if bits > 0 {
        out.push(char::from(ALPHABET[((buf << (5 - bits)) & 31) as usize]));
    }
    out
}

/// Decodes base32 (case-insensitive, spaces and padding ignored).
#[must_use]
pub fn base32_decode(s: &str) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    let (mut buf, mut bits) = (0u32, 0u32);
    for c in s.chars().filter(|c| !matches!(c, ' ' | '=')) {
        let v = match c.to_ascii_uppercase() {
            c @ 'A'..='Z' => c as u32 - 'A' as u32,
            c @ '2'..='7' => c as u32 - '2' as u32 + 26,
            _ => return None,
        };
        buf = (buf << 5) | v;
        bits += 5;
        if bits >= 8 {
            bits -= 8;
            out.push(((buf >> bits) & 0xff) as u8);
        }
    }
    Some(out)
}

/// A new random seed.
pub fn new_secret() -> Result<Secret<Vec<u8>>, String> {
    let mut s = vec![0u8; SECRET_LEN];
    getrandom::fill(&mut s).map_err(|e| e.to_string())?;
    Ok(Secret::new(s))
}

/// The step a Unix time falls into.
#[must_use]
pub const fn step_at(unix: i64) -> i64 {
    unix.div_euclid(STEP_SECS)
}

/// The code for `step`.
#[must_use]
pub fn code_at(secret: &[u8], step: i64) -> String {
    let key = hmac::Key::new(hmac::HMAC_SHA1_FOR_LEGACY_USE_ONLY, secret);
    let tag = hmac::sign(&key, &step.to_be_bytes());
    let h = tag.as_ref();
    let off = usize::from(h[h.len() - 1] & 0x0f);
    let bin = (u32::from(h[off] & 0x7f) << 24)
        | (u32::from(h[off + 1]) << 16)
        | (u32::from(h[off + 2]) << 8)
        | u32::from(h[off + 3]);
    format!(
        "{:0width$}",
        bin % 10u32.pow(DIGITS),
        width = DIGITS as usize
    )
}

/// Normalizes user input: digits only (spaces and dashes removed).
#[must_use]
pub fn normalize(code: &str) -> Option<String> {
    let digits: String = code.chars().filter(|c| !matches!(c, ' ' | '-')).collect();
    (digits.len() == DIGITS as usize && digits.bytes().all(|b| b.is_ascii_digit()))
        .then_some(digits)
}

/// Checks `code` at Unix time `now` against steps now-1..=now+1 that are
/// after `last_used_step`. Returns the matching step, to be stored as the
/// new `last_used_step`.
#[must_use]
pub fn verify(secret: &[u8], code: &str, now: i64, last_used_step: i64) -> Option<i64> {
    let code = normalize(code)?;
    let current = step_at(now);
    let mut found = None;
    for step in [current - 1, current, current + 1] {
        // Compare all candidates in constant time per comparison.
        let ok = subtle::ConstantTimeEq::ct_eq(code_at(secret, step).as_bytes(), code.as_bytes());
        if bool::from(ok) && step > last_used_step && found.is_none() {
            found = Some(step);
        }
    }
    found
}

/// The `otpauth://` URI authenticator apps scan.
#[must_use]
pub fn otpauth_uri(secret: &[u8], issuer: &str, account: &str) -> String {
    let enc = |s: &str| percent_encode(s);
    format!(
        "otpauth://totp/{}:{}?secret={}&issuer={}&algorithm=SHA1&digits=6&period=30",
        enc(issuer),
        enc(account),
        base32(secret),
        enc(issuer)
    )
}

fn percent_encode(s: &str) -> String {
    s.bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
                char::from(b).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}

/// Number of recovery codes issued at once.
pub const RECOVERY_CODES: usize = 10;

/// New recovery codes: 10 characters of base32 each, shown as `xxxxx-xxxxx`.
pub fn new_recovery_codes() -> Result<Vec<Secret<String>>, String> {
    (0..RECOVERY_CODES)
        .map(|_| {
            let mut b = [0u8; 7];
            getrandom::fill(&mut b).map_err(|e| e.to_string())?;
            let s = base32(&b).to_ascii_lowercase();
            Ok(Secret::new(format!("{}-{}", &s[..5], &s[5..10])))
        })
        .collect()
}

/// Canonical form of a recovery code as typed (case, spaces, and dashes ignored).
#[must_use]
pub fn normalize_recovery_code(code: &str) -> Option<String> {
    let c: String = code
        .chars()
        .filter(|c| !matches!(c, ' ' | '-'))
        .map(|c| c.to_ascii_lowercase())
        .collect();
    (c.len() == 10
        && c.bytes()
            .all(|b| b.is_ascii_lowercase() || (b'2'..=b'7').contains(&b)))
    .then(|| format!("{}-{}", &c[..5], &c[5..]))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RFC 6238 appendix B (SHA-1, 8-digit vectors truncated to 6 digits).
    #[test]
    fn rfc6238_vectors() {
        let secret = b"12345678901234567890";
        for (t, code8) in [
            (59, "94287082"),
            (1_111_111_109, "07081804"),
            (1_111_111_111, "14050471"),
            (1_234_567_890, "89005924"),
            (2_000_000_000, "69279037"),
        ] {
            assert_eq!(code_at(secret, step_at(t)), code8[2..], "t={t}");
        }
    }

    #[test]
    fn base32_matches_rfc4648() {
        assert_eq!(base32(b""), "");
        assert_eq!(base32(b"f"), "MY");
        assert_eq!(base32(b"foobar"), "MZXW6YTBOI");
        assert_eq!(
            base32(b"12345678901234567890"),
            "GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ"
        );
        for v in [&b""[..], b"f", b"fo", b"foobar", b"12345678901234567890"] {
            assert_eq!(base32_decode(&base32(v)).unwrap(), v);
        }
        assert_eq!(base32_decode("mzxw 6ytb oi").unwrap(), b"foobar");
        assert!(base32_decode("M1").is_none());
    }

    #[test]
    fn drift_and_replay() {
        let s = b"12345678901234567890";
        let now = 1_700_000_000;
        let step = step_at(now);
        assert_eq!(verify(s, &code_at(s, step), now, 0), Some(step));
        assert_eq!(verify(s, &code_at(s, step - 1), now, 0), Some(step - 1));
        assert_eq!(verify(s, &code_at(s, step + 1), now, 0), Some(step + 1));
        assert_eq!(verify(s, &code_at(s, step - 2), now, 0), None);
        assert_eq!(verify(s, &code_at(s, step + 2), now, 0), None);
        // The same code twice in one step: the second time it's at the last
        // used step and refused.
        assert_eq!(verify(s, &code_at(s, step), now, step), None);
        // An older code after a newer one was used is refused too.
        assert_eq!(verify(s, &code_at(s, step - 1), now, step), None);
        assert_eq!(verify(s, "12345", now, 0), None);
        assert_eq!(verify(s, "abcdef", now, 0), None);
        let spaced = code_at(s, step);
        let spaced = format!("{} {}", &spaced[..3], &spaced[3..]);
        assert_eq!(verify(s, &spaced, now, 0), Some(step));
    }

    #[test]
    fn recovery_codes() {
        let codes = new_recovery_codes().unwrap();
        assert_eq!(codes.len(), 10);
        for c in &codes {
            assert_eq!(
                normalize_recovery_code(c.expose()).as_deref(),
                Some(c.expose().as_str())
            );
            let loose = c.expose().replace('-', " ").to_uppercase();
            assert_eq!(
                normalize_recovery_code(&loose).as_deref(),
                Some(c.expose().as_str())
            );
        }
        assert!(normalize_recovery_code("abc").is_none());
        assert!(normalize_recovery_code("abcde-fgh18").is_none());
    }

    #[test]
    fn uri() {
        let u = otpauth_uri(b"12345678901234567890", "Slath git.example.org", "alice");
        assert!(u.starts_with("otpauth://totp/Slath%20git.example.org:alice?secret=GEZDGNBV"));
    }
}
