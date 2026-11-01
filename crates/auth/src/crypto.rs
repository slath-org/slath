// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Envelope encryption of secrets at rest (TOTP seeds, webhook secrets, CI
//! secrets): AES-256-GCM under a key derived from the master key.
//!
//! Every ciphertext is bound to where it is stored through the associated
//! data (table, column, and row ID), so a value copied into another row or
//! column fails to decrypt instead of being accepted. The ID of the master
//! key is stored next to each value; rotation re-encrypts rows whose key ID
//! isn't the current one, using the old keys listed in
//! `secrets.old_key_files`.
//!
//! Format: `0x01 || nonce (12 bytes) || ciphertext || tag (16 bytes)`.

use ring::aead::{AES_256_GCM, Aad, LessSafeKey, NONCE_LEN, Nonce, UnboundKey};
use slath_core::Secret;

use crate::MasterKey;

const VERSION: u8 = 1;
/// HKDF purpose of the data key; changing it makes every stored value unreadable.
const PURPOSE: &str = "secrets-at-rest v1";

#[derive(Debug, thiserror::Error)]
pub enum CryptoError {
    /// The value was encrypted with a key that isn't loaded.
    #[error(
        "the value was encrypted with master key {0}, which is not loaded \
         (add its file to secrets.old_key_files)"
    )]
    UnknownKey(String),
    /// Wrong key, wrong location (associated data), or tampered data.
    #[error("the encrypted value cannot be decrypted")]
    Decrypt,
    #[error("cannot generate random bytes: {0}")]
    Random(String),
}

/// Where a ciphertext lives. It is authenticated with the value.
#[derive(Debug, Clone, Copy)]
pub struct Location<'a> {
    pub table: &'a str,
    pub column: &'a str,
    /// The row's primary key in text form (e.g. a UUID).
    pub row: &'a str,
}

impl Location<'_> {
    fn aad(&self) -> Vec<u8> {
        let mut v = Vec::with_capacity(16 + self.table.len() + self.column.len() + self.row.len());
        v.extend_from_slice(b"slath/v1\0");
        for part in [self.table, self.column, self.row] {
            v.extend_from_slice(part.as_bytes());
            v.push(0);
        }
        v
    }
}

/// An encrypted value and the ID of the master key it was sealed with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sealed {
    pub ciphertext: Vec<u8>,
    pub key_id: String,
}

struct DataKey {
    id: String,
    key: LessSafeKey,
}

impl DataKey {
    fn new(master: &MasterKey) -> Self {
        let derived = master.derive(PURPOSE);
        // A 32-byte key always has the right length for AES-256.
        let key = match UnboundKey::new(&AES_256_GCM, derived.expose()) {
            Ok(k) => LessSafeKey::new(k),
            Err(_) => unreachable!("AES-256 keys are 32 bytes"),
        };
        Self {
            id: master.id().to_owned(),
            key,
        }
    }
}

/// The current master key plus old keys kept for decryption during rotation.
pub struct Envelope {
    current: DataKey,
    old: Vec<DataKey>,
}

impl std::fmt::Debug for Envelope {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Envelope")
            .field("current", &self.current.id)
            .field("old", &self.old.iter().map(|k| &k.id).collect::<Vec<_>>())
            .finish()
    }
}

impl Envelope {
    #[must_use]
    pub fn new(current: &MasterKey, old: &[MasterKey]) -> Self {
        Self {
            current: DataKey::new(current),
            old: old
                .iter()
                .filter(|k| k.id() != current.id())
                .map(DataKey::new)
                .collect(),
        }
    }

    /// ID of the key new values are sealed with.
    #[must_use]
    pub fn current_id(&self) -> &str {
        &self.current.id
    }

    /// IDs of every loaded key, current first.
    #[must_use]
    pub fn key_ids(&self) -> Vec<&str> {
        std::iter::once(self.current.id.as_str())
            .chain(self.old.iter().map(|k| k.id.as_str()))
            .collect()
    }

    /// Encrypts `plaintext` for storage at `at` with the current key.
    pub fn seal(&self, plaintext: &[u8], at: Location<'_>) -> Result<Sealed, CryptoError> {
        let mut nonce = [0u8; NONCE_LEN];
        getrandom::fill(&mut nonce).map_err(|e| CryptoError::Random(e.to_string()))?;
        let mut buf = plaintext.to_vec();
        self.current
            .key
            .seal_in_place_append_tag(
                Nonce::assume_unique_for_key(nonce),
                Aad::from(at.aad()),
                &mut buf,
            )
            .map_err(|_| CryptoError::Decrypt)?;
        let mut out = Vec::with_capacity(1 + NONCE_LEN + buf.len());
        out.push(VERSION);
        out.extend_from_slice(&nonce);
        out.extend_from_slice(&buf);
        Ok(Sealed {
            ciphertext: out,
            key_id: self.current.id.clone(),
        })
    }

    /// Decrypts a value stored at `at`.
    pub fn open(
        &self,
        ciphertext: &[u8],
        key_id: &str,
        at: Location<'_>,
    ) -> Result<Secret<Vec<u8>>, CryptoError> {
        let key = std::iter::once(&self.current)
            .chain(&self.old)
            .find(|k| k.id == key_id)
            .ok_or_else(|| CryptoError::UnknownKey(key_id.to_owned()))?;
        let (&version, rest) = ciphertext.split_first().ok_or(CryptoError::Decrypt)?;
        if version != VERSION || rest.len() < NONCE_LEN {
            return Err(CryptoError::Decrypt);
        }
        let (nonce, body) = rest.split_at(NONCE_LEN);
        let nonce = Nonce::try_assume_unique_for_key(nonce).map_err(|_| CryptoError::Decrypt)?;
        let mut buf = body.to_vec();
        let plain = key
            .key
            .open_in_place(nonce, Aad::from(at.aad()), &mut buf)
            .map_err(|_| CryptoError::Decrypt)?;
        Ok(Secret::new(plain.to_vec()))
    }

    /// Decrypts with any loaded key and seals again with the current key.
    /// Returns `None` if the value already uses the current key.
    pub fn reseal(
        &self,
        ciphertext: &[u8],
        key_id: &str,
        at: Location<'_>,
    ) -> Result<Option<Sealed>, CryptoError> {
        if key_id == self.current.id {
            return Ok(None);
        }
        let plain = self.open(ciphertext, key_id, at)?;
        self.seal(plain.expose(), at).map(Some)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const AT: Location<'static> = Location {
        table: "totp_credentials",
        column: "secret_enc",
        row: "0190d3a4-0000-7000-8000-000000000001",
    };

    #[test]
    fn round_trip() {
        let k = MasterKey::generate().unwrap();
        let env = Envelope::new(&k, &[]);
        let s = env.seal(b"hunter2", AT).unwrap();
        assert_eq!(s.key_id, k.id());
        assert!(!s.ciphertext.windows(7).any(|w| w == b"hunter2"));
        assert_eq!(
            env.open(&s.ciphertext, &s.key_id, AT).unwrap().expose(),
            b"hunter2"
        );
        // Fresh nonce every time.
        assert_ne!(env.seal(b"hunter2", AT).unwrap().ciphertext, s.ciphertext);
    }

    #[test]
    fn ciphertext_is_bound_to_its_row_and_column() {
        let env = Envelope::new(&MasterKey::generate().unwrap(), &[]);
        let s = env.seal(b"seed", AT).unwrap();
        for other in [
            Location {
                row: "0190d3a4-0000-7000-8000-000000000002",
                ..AT
            },
            Location {
                column: "other",
                ..AT
            },
            Location {
                table: "webhooks",
                ..AT
            },
        ] {
            assert!(matches!(
                env.open(&s.ciphertext, &s.key_id, other),
                Err(CryptoError::Decrypt)
            ));
        }
        let mut tampered = s.ciphertext.clone();
        let last = tampered.len() - 1;
        tampered[last] ^= 1;
        assert!(env.open(&tampered, &s.key_id, AT).is_err());
        assert!(env.open(&[], &s.key_id, AT).is_err());
        assert!(env.open(&[1, 2, 3], &s.key_id, AT).is_err());
    }

    #[test]
    fn rotation() {
        let old = MasterKey::generate().unwrap();
        let new = MasterKey::generate().unwrap();
        let s = Envelope::new(&old, &[]).seal(b"seed", AT).unwrap();
        // Without the old key the value is unreadable, with a clear error.
        let only_new = Envelope::new(&new, &[]);
        assert!(matches!(
            only_new.open(&s.ciphertext, &s.key_id, AT),
            Err(CryptoError::UnknownKey(_))
        ));
        let env = Envelope::new(&new, &[old]);
        assert_eq!(env.key_ids().len(), 2);
        let r = env.reseal(&s.ciphertext, &s.key_id, AT).unwrap().unwrap();
        assert_eq!(r.key_id, new.id());
        assert_eq!(
            only_new
                .open(&r.ciphertext, &r.key_id, AT)
                .unwrap()
                .expose(),
            b"seed"
        );
        // Idempotent: a value under the current key is left alone.
        assert!(env.reseal(&r.ciphertext, &r.key_id, AT).unwrap().is_none());
    }
}
