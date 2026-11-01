// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! The instance master key: 32 random bytes, stored base64-encoded in a
//! file with mode 0600. Other keys (CSRF, cursor signing, encryption of
//! secrets at rest) are derived from it, so it is the only key material an
//! operator has to back up.

use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::Path;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use hkdf::Hkdf;
use sha2::{Digest, Sha256};
use slath_core::Secret;

/// Length of the key in bytes.
pub const KEY_LEN: usize = 32;

#[derive(Debug, thiserror::Error)]
pub enum KeyError {
    #[error("cannot read {path}: {source}")]
    Io {
        path: String,
        source: std::io::Error,
    },
    #[error("{0} must not be readable by group or others (chmod 600)")]
    Permissions(String),
    #[error("{0} does not contain a valid key (generate one with `slath admin gen-key`)")]
    Invalid(String),
    #[error("cannot generate random bytes: {0}")]
    Random(String),
}

/// A loaded master key.
pub struct MasterKey {
    key: Secret<[u8; KEY_LEN]>,
    id: String,
}

impl std::fmt::Debug for MasterKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "MasterKey({})", self.id)
    }
}

impl MasterKey {
    /// Generates a new random key.
    pub fn generate() -> Result<Self, KeyError> {
        let mut key = [0u8; KEY_LEN];
        getrandom::fill(&mut key).map_err(|e| KeyError::Random(e.to_string()))?;
        Ok(Self::from_bytes(key))
    }

    fn from_bytes(key: [u8; KEY_LEN]) -> Self {
        let digest = Sha256::digest(key);
        let id = digest[..4].iter().map(|b| format!("{b:02x}")).collect();
        Self {
            key: Secret::new(key),
            id,
        }
    }

    /// Parses the file format: base64 of the key, surrounding whitespace ignored.
    pub fn parse(text: &str) -> Option<Self> {
        let bytes = STANDARD.decode(text.trim()).ok()?;
        let key: [u8; KEY_LEN] = bytes.try_into().ok()?;
        Some(Self::from_bytes(key))
    }

    /// The text written to a key file (with a trailing newline).
    #[must_use]
    pub fn encode(&self) -> String {
        format!("{}\n", STANDARD.encode(self.key.expose()))
    }

    /// Reads a key file, refusing files readable by group or others.
    pub fn load(path: &Path) -> Result<Self, KeyError> {
        let shown = path.display().to_string();
        let meta = std::fs::metadata(path).map_err(|source| KeyError::Io {
            path: shown.clone(),
            source,
        })?;
        if meta.permissions().mode() & 0o077 != 0 {
            return Err(KeyError::Permissions(shown));
        }
        let text = std::fs::read_to_string(path).map_err(|source| KeyError::Io {
            path: shown.clone(),
            source,
        })?;
        Self::parse(&text).ok_or(KeyError::Invalid(shown))
    }

    /// Loads the key at `path`, or creates it (mode 0600) if it doesn't exist.
    pub fn load_or_create(path: &Path) -> Result<Self, KeyError> {
        if path.exists() {
            return Self::load(path);
        }
        let key = Self::generate()?;
        let shown = path.display().to_string();
        let io = |source| KeyError::Io {
            path: shown.clone(),
            source,
        };
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)
            .map_err(io)?;
        std::io::Write::write_all(&mut f, key.encode().as_bytes()).map_err(io)?;
        Ok(key)
    }

    /// Short identifier stored next to data encrypted with this key.
    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Derives a 32-byte subkey for one purpose (HKDF-SHA256).
    #[must_use]
    pub fn derive(&self, purpose: &str) -> Secret<[u8; 32]> {
        let hk = Hkdf::<Sha256>::new(Some(b"slath master key v1"), self.key.expose());
        let mut out = [0u8; 32];
        // 32 bytes is far below HKDF's maximum output length.
        let _ = hk.expand(purpose.as_bytes(), &mut out);
        Secret::new(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_and_derivation() {
        let k = MasterKey::generate().unwrap();
        let text = k.encode();
        let back = MasterKey::parse(&text).unwrap();
        assert_eq!(k.id(), back.id());
        assert_eq!(k.derive("csrf").expose(), back.derive("csrf").expose());
        assert_ne!(k.derive("csrf").expose(), k.derive("cursor").expose());
        assert!(MasterKey::parse("short").is_none());
        assert!(!format!("{k:?}").contains(&text.trim()[..10]));
    }

    #[test]
    fn file_permissions_are_enforced() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("master.key");
        let k = MasterKey::load_or_create(&p).unwrap();
        assert_eq!(
            std::fs::metadata(&p).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(MasterKey::load(&p).unwrap().id(), k.id());
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(matches!(MasterKey::load(&p), Err(KeyError::Permissions(_))));
    }
}
