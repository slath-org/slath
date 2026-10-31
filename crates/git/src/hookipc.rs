// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Messages between `slath hook` (spawned by Git) and `slath serve` over the
//! IPC Unix socket.
//!
//! Framing: the client writes one JSON request line, the server answers with
//! one JSON response line. Ref names are bytes and travel base64-encoded.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;

use serde::{Deserialize, Serialize};

/// Maximum size of one message line.
pub const MAX_MESSAGE: usize = 16 * 1024 * 1024;
/// How long a hook waits for the server before failing closed.
pub const HOOK_TIMEOUT: Duration = Duration::from_secs(5);

/// One `<old> <new> <ref>` line from the hook's stdin.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RefUpdate {
    pub old: String,
    pub new: String,
    #[serde(with = "b64")]
    pub name: Vec<u8>,
}

impl RefUpdate {
    /// Parses a hook stdin line. Returns `None` for malformed input.
    #[must_use]
    pub fn parse_line(line: &[u8]) -> Option<Self> {
        let mut parts = line.splitn(3, |&b| b == b' ');
        let old = std::str::from_utf8(parts.next()?).ok()?;
        let new = std::str::from_utf8(parts.next()?).ok()?;
        let name = parts.next()?;
        let hex = |s: &str| s.len() == 40 && s.bytes().all(|b| b.is_ascii_hexdigit());
        (hex(old) && hex(new) && !name.is_empty()).then(|| Self {
            old: old.to_owned(),
            new: new.to_owned(),
            name: name.to_vec(),
        })
    }
}

/// Quarantine directories Git sets for pre-receive (objects not yet accepted).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Quarantine {
    pub object_directory: Option<String>,
    pub alternate_object_directories: Option<String>,
    pub quarantine_path: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Request {
    PreReceive {
        token: String,
        updates: Vec<RefUpdate>,
        quarantine: Quarantine,
    },
    PostReceive {
        token: String,
        updates: Vec<RefUpdate>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Response {
    /// For pre-receive: whether the whole push may proceed.
    pub allow: bool,
    /// Lines to show the user (Git prefixes them with `remote:`).
    pub messages: Vec<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum IpcError {
    #[error("cannot reach the Slath server: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid message: {0}")]
    Json(#[from] serde_json::Error),
    #[error("message too large")]
    TooLarge,
    #[error("server closed the connection without answering")]
    NoAnswer,
}

/// Sends a request and waits for the answer (blocking; used by `slath hook`).
pub fn call(socket: &Path, request: &Request, timeout: Duration) -> Result<Response, IpcError> {
    let mut stream = UnixStream::connect(socket)?;
    stream.set_read_timeout(Some(timeout))?;
    stream.set_write_timeout(Some(timeout))?;
    let mut line = serde_json::to_vec(request)?;
    line.push(b'\n');
    if line.len() > MAX_MESSAGE {
        return Err(IpcError::TooLarge);
    }
    stream.write_all(&line)?;
    let mut reader = BufReader::new(std::io::Read::take(stream, MAX_MESSAGE as u64));
    let mut answer = Vec::new();
    reader.read_until(b'\n', &mut answer)?;
    if answer.last() != Some(&b'\n') {
        return Err(IpcError::NoAnswer);
    }
    Ok(serde_json::from_slice(&answer)?)
}

mod b64 {
    use base64::Engine;
    use base64::engine::general_purpose::STANDARD;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(v: &[u8], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&STANDARD.encode(v))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<u8>, D::Error> {
        let s = String::deserialize(d)?;
        STANDARD.decode(s).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_hook_lines() {
        let z = "0".repeat(40);
        let a = "a".repeat(40);
        let u = RefUpdate::parse_line(format!("{z} {a} refs/heads/m\u{e9}").as_bytes()).unwrap();
        assert_eq!(u.name, "refs/heads/m\u{e9}".as_bytes());
        assert!(RefUpdate::parse_line(b"x y z").is_none());
        assert!(RefUpdate::parse_line(format!("{z} {a} ").as_bytes()).is_none());
    }

    #[test]
    fn non_utf8_ref_names_round_trip() {
        let req = Request::PostReceive {
            token: "t".into(),
            updates: vec![RefUpdate {
                old: "0".repeat(40),
                new: "1".repeat(40),
                name: vec![b'r', 0xff],
            }],
        };
        let json = serde_json::to_string(&req).unwrap();
        assert_eq!(serde_json::from_str::<Request>(&json).unwrap(), req);
    }
}
