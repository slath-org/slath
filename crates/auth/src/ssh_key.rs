// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Parsing and validating SSH public keys in `authorized_keys` format.
//!
//! Accepted: `ssh-ed25519`, `sk-ssh-ed25519@openssh.com`,
//! `ecdsa-sha2-nistp256/384/521`, `sk-ecdsa-sha2-nistp256@openssh.com`, and
//! `ssh-rsa` with at least 3072 bits. Rejected: DSA, shorter RSA keys,
//! certificates (Slath authorizes keys, not certificate authorities), and
//! anything whose wire format doesn't match its declared type.

use base64::Engine;
use base64::engine::general_purpose::{STANDARD, STANDARD_NO_PAD};
use sha2::{Digest, Sha256};

/// Minimum RSA modulus size.
pub const MIN_RSA_BITS: u32 = 3072;
/// Longest accepted key line (an RSA-16384 key is about 2.8 KB).
pub const MAX_LINE: usize = 16 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum KeyError {
    #[error("Paste a public key that starts with its type, e.g. \"ssh-ed25519 AAAA…\".")]
    Format,
    #[error("DSA keys are insecure and not accepted. Use an Ed25519 key.")]
    Dsa,
    #[error("RSA keys need at least {MIN_RSA_BITS} bits; this one has {0}. Use an Ed25519 key.")]
    RsaTooShort(u32),
    #[error("Certificates aren't accepted. Add the public key itself.")]
    Certificate,
    #[error("Unsupported key type {0}.")]
    UnsupportedType(String),
    #[error("The key data is damaged or doesn't match its type.")]
    Malformed,
    #[error("That looks like a private key. Add the public key (the .pub file) instead.")]
    PrivateKey,
}

/// A validated public key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublicKey {
    /// E.g. `ssh-ed25519`.
    pub key_type: String,
    /// The key blob, base64 as in `authorized_keys`.
    pub base64: String,
    /// `SHA256:` + unpadded base64 of the blob's SHA-256 (as `ssh-keygen -l`).
    pub fingerprint: String,
    /// Key size in bits (RSA modulus, curve size, 256 for Ed25519).
    pub bits: u32,
    /// The comment after the key, if any.
    pub comment: Option<String>,
}

/// Reads SSH wire-format fields.
struct Reader<'a>(&'a [u8]);

impl<'a> Reader<'a> {
    fn string(&mut self) -> Option<&'a [u8]> {
        let len = u32::from_be_bytes(self.0.get(..4)?.try_into().ok()?) as usize;
        let s = self.0.get(4..4 + len)?;
        self.0 = &self.0[4 + len..];
        Some(s)
    }

    fn done(&self) -> bool {
        self.0.is_empty()
    }
}

/// Bit length of an mpint (big-endian, possibly with a leading zero byte).
fn mpint_bits(m: &[u8]) -> u32 {
    let m = match m.iter().position(|&b| b != 0) {
        Some(i) => &m[i..],
        None => return 0,
    };
    (m.len() as u32 - 1) * 8 + (8 - m[0].leading_zeros())
}

fn validate(key_type: &str, blob: &[u8]) -> Result<u32, KeyError> {
    let mut r = Reader(blob);
    let inner = r.string().ok_or(KeyError::Malformed)?;
    if inner != key_type.as_bytes() {
        return Err(KeyError::Malformed);
    }
    let bits = match key_type {
        "ssh-ed25519" => {
            if r.string().ok_or(KeyError::Malformed)?.len() != 32 {
                return Err(KeyError::Malformed);
            }
            256
        }
        "sk-ssh-ed25519@openssh.com" => {
            if r.string().ok_or(KeyError::Malformed)?.len() != 32 {
                return Err(KeyError::Malformed);
            }
            r.string().ok_or(KeyError::Malformed)?; // application
            256
        }
        "ssh-rsa" => {
            let e = r.string().ok_or(KeyError::Malformed)?;
            let n = r.string().ok_or(KeyError::Malformed)?;
            if e.is_empty() || n.is_empty() {
                return Err(KeyError::Malformed);
            }
            let bits = mpint_bits(n);
            if bits < MIN_RSA_BITS {
                return Err(KeyError::RsaTooShort(bits));
            }
            bits
        }
        t if t.starts_with("ecdsa-sha2-nistp") || t == "sk-ecdsa-sha2-nistp256@openssh.com" => {
            let curve = t
                .trim_start_matches("sk-")
                .trim_start_matches("ecdsa-sha2-")
                .trim_end_matches("@openssh.com");
            let (bits, point_len) = match curve {
                "nistp256" => (256, 65),
                "nistp384" => (384, 97),
                "nistp521" => (521, 133),
                _ => return Err(KeyError::UnsupportedType(t.to_owned())),
            };
            if r.string().ok_or(KeyError::Malformed)? != curve.as_bytes() {
                return Err(KeyError::Malformed);
            }
            let q = r.string().ok_or(KeyError::Malformed)?;
            if q.len() != point_len || q[0] != 4 {
                return Err(KeyError::Malformed);
            }
            if t.starts_with("sk-") {
                r.string().ok_or(KeyError::Malformed)?; // application
            }
            bits
        }
        _ => return Err(KeyError::UnsupportedType(key_type.to_owned())),
    };
    if !r.done() {
        return Err(KeyError::Malformed);
    }
    Ok(bits)
}

/// Parses one `authorized_keys`-style line: `<type> <base64> [comment]`.
/// Options before the type (`command="…"`, `restrict`) aren't accepted.
pub fn parse(line: &str) -> Result<PublicKey, KeyError> {
    let line = line.trim();
    if line.len() > MAX_LINE {
        return Err(KeyError::Format);
    }
    if line.contains("PRIVATE KEY") {
        return Err(KeyError::PrivateKey);
    }
    // Fields are separated by runs of whitespace; the comment is the rest.
    let (key_type, rest) = line
        .split_once(char::is_whitespace)
        .ok_or(KeyError::Format)?;
    let rest = rest.trim_start();
    let (data, comment) = match rest.split_once(char::is_whitespace) {
        Some((d, c)) => (d, Some(c)),
        None => (rest, None),
    };
    let comment = comment
        .map(|c| {
            c.trim()
                .chars()
                .filter(|c| !c.is_control())
                .take(200)
                .collect::<String>()
        })
        .filter(|c| !c.is_empty());
    if key_type == "ssh-dss" {
        return Err(KeyError::Dsa);
    }
    if key_type.contains("-cert-v01@openssh.com") {
        return Err(KeyError::Certificate);
    }
    if !key_type
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b"-@.".contains(&b))
    {
        return Err(KeyError::Format);
    }
    if !data
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b"+/=".contains(&b))
    {
        return Err(KeyError::Format);
    }
    let blob = STANDARD.decode(data).map_err(|_| KeyError::Malformed)?;
    let bits = validate(key_type, &blob)?;
    let fingerprint = format!("SHA256:{}", STANDARD_NO_PAD.encode(Sha256::digest(&blob)));
    Ok(PublicKey {
        key_type: key_type.to_owned(),
        // Re-encoded, so only canonical base64 is ever stored or printed.
        base64: STANDARD.encode(&blob),
        fingerprint,
        bits,
        comment,
    })
}

/// True if `fp` is a well-formed `SHA256:` fingerprint (43 base64 characters).
#[must_use]
pub fn is_fingerprint(fp: &str) -> bool {
    fp.strip_prefix("SHA256:").is_some_and(|b| {
        b.len() == 43
            && b.bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'+' || c == b'/')
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn data(name: &str) -> String {
        let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/data/ssh")
            .join(name);
        std::fs::read_to_string(p).unwrap()
    }

    #[test]
    fn accepted_keys_match_ssh_keygen_fingerprints() {
        for (name, bits) in [
            ("ed25519", 256),
            ("rsa3072", 3072),
            ("ecdsa", 256),
            ("ecdsa384", 384),
            ("ecdsa521", 521),
        ] {
            let k = parse(&data(&format!("{name}.pub"))).unwrap_or_else(|e| panic!("{name}: {e}"));
            assert_eq!(k.fingerprint, data(&format!("{name}.fp")).trim(), "{name}");
            assert_eq!(k.bits, bits, "{name}");
            assert_eq!(k.comment.as_deref(), Some(format!("test-{name}").as_str()));
            assert!(is_fingerprint(&k.fingerprint));
        }
    }

    #[test]
    fn rejected_keys() {
        assert_eq!(parse(&data("rsa.pub")), Err(KeyError::RsaTooShort(2048)));
        assert_eq!(parse(&data("rsa3072-cert.pub")), Err(KeyError::Certificate));
        assert_eq!(parse("ssh-dss AAAAB3NzaC1kc3MAAACBAP"), Err(KeyError::Dsa));
        assert_eq!(parse(""), Err(KeyError::Format));
        assert_eq!(parse("ssh-ed25519"), Err(KeyError::Format));
        assert_eq!(
            parse("-----BEGIN OPENSSH PRIVATE KEY-----"),
            Err(KeyError::PrivateKey)
        );
        assert_eq!(parse("ssh-ed25519 !!!!"), Err(KeyError::Format));
        assert_eq!(parse("ssh-ed25519 AAAA"), Err(KeyError::Malformed));
        // The declared type must match the blob.
        let ed = data("ed25519.pub");
        let blob = ed.split_whitespace().nth(1).unwrap();
        assert_eq!(parse(&format!("ssh-rsa {blob}")), Err(KeyError::Malformed));
        // Options before the key and shell metacharacters are refused.
        assert_eq!(
            parse(&format!("command=\"sh\" ssh-ed25519 {blob}")),
            Err(KeyError::Format)
        );
        assert_eq!(
            parse(&format!("ssh-ed25519\"; {blob}")),
            Err(KeyError::Format)
        );
        // Trailing garbage in the blob.
        let mut raw = STANDARD.decode(blob).unwrap();
        raw.push(0);
        assert_eq!(
            parse(&format!("ssh-ed25519 {}", STANDARD.encode(raw))),
            Err(KeyError::Malformed)
        );
        assert!(matches!(
            parse("ssh-foo AAAA"),
            Err(KeyError::Malformed | KeyError::UnsupportedType(_))
        ));
    }

    #[test]
    fn whitespace_runs_separate_fields() {
        let k = data("ed25519.pub");
        let mut parts = k.split_whitespace();
        let (t, b) = (parts.next().unwrap(), parts.next().unwrap());
        let parsed = parse(&format!("{t} \t  {b}   my  laptop  ")).unwrap();
        assert_eq!(parsed.fingerprint, data("ed25519.fp").trim());
        assert_eq!(parsed.comment.as_deref(), Some("my  laptop"));
    }

    #[test]
    fn fingerprints() {
        assert!(!is_fingerprint("SHA256:short"));
        assert!(!is_fingerprint(&format!("SHA256:{}%", "a".repeat(42))));
        assert!(!is_fingerprint(&format!("MD5:{}", "a".repeat(43))));
    }
}
