// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Messages between the helper commands (`slath hook`, `slath ssh-keys`,
//! `slath ssh-serve`) and `slath serve` over the IPC Unix socket. Helpers
//! never hold database credentials or touch repositories; everything they
//! need comes from here.
//!
//! Framing: the client writes one JSON request line, the server answers with
//! one JSON response line. Ref names are bytes and travel base64-encoded.
//!
//! [`Request::SshSession`] continues after an allowing answer: the client
//! sends one byte carrying its stdin, stdout, and stderr as `SCM_RIGHTS`, the
//! server runs Git on them, and answers a second time with Git's exit code.
//! So Git runs as the server's user, and the SSH login only needs to be
//! allowed to connect to the socket.

use std::io::{BufRead, BufReader, IoSlice, IoSliceMut, Write};
use std::mem::MaybeUninit;
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;

use rustix::net::{
    RecvAncillaryBuffer, RecvAncillaryMessage, RecvFlags, ReturnFlags, SendAncillaryBuffer,
    SendAncillaryMessage, SendFlags,
};
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
    /// `AuthorizedKeysCommand`: find the key with this SHA-256 fingerprint.
    LookupKey { fingerprint: String },
    /// SSH forced command: run `service` on the repository for this key, on
    /// the file descriptors that follow an allowing answer.
    SshSession {
        key_id: String,
        service: String,
        namespace: String,
        repo: String,
        /// The client's `GIT_PROTOCOL`, if any (the runner validates it).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        git_protocol: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Response {
    /// Whether the request is granted (for pre-receive: the whole push).
    pub allow: bool,
    /// Lines to show the user (for hooks, Git prefixes them with `remote:`).
    pub messages: Vec<String>,
    /// Answer to [`Request::LookupKey`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<AuthorizedKey>,
    /// Second answer to [`Request::SshSession`]: Git's exit code.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
}

impl Response {
    #[must_use]
    pub fn allow() -> Self {
        Self {
            allow: true,
            messages: Vec::new(),
            key: None,
            exit_code: None,
        }
    }

    #[must_use]
    pub fn deny(message: impl Into<String>) -> Self {
        Self {
            allow: false,
            messages: vec![message.into()],
            key: None,
            exit_code: None,
        }
    }
}

/// A public key known to the server.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthorizedKey {
    pub key_id: String,
    /// E.g. `ssh-ed25519`.
    pub key_type: String,
    /// The base64 key blob as in `authorized_keys`.
    pub key_base64: String,
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
    #[error("expected {expected} file descriptors, got {got}")]
    Descriptors { expected: usize, got: usize },
}

/// Sends a request and waits for the answer (blocking; used by `slath hook`).
pub fn call(socket: &Path, request: &Request, timeout: Duration) -> Result<Response, IpcError> {
    let stream = connect(socket, request, timeout)?;
    read_response(&stream)
}

/// Runs an SSH session: sends `request` (a [`Request::SshSession`]), and if
/// the server allows it, hands it `stdio` (stdin, stdout, stderr) and waits
/// without a timeout until Git is done. Returns the denial or the final
/// answer with the exit code.
pub fn ssh_session(
    socket: &Path,
    request: &Request,
    stdio: [BorrowedFd<'_>; 3],
    timeout: Duration,
) -> Result<Response, IpcError> {
    let stream = connect(socket, request, timeout)?;
    let first = read_response(&stream)?;
    if !first.allow {
        return Ok(first);
    }
    send_fds(&stream, &stdio)?;
    stream.set_read_timeout(None)?;
    read_response(&stream)
}

fn connect(socket: &Path, request: &Request, timeout: Duration) -> Result<UnixStream, IpcError> {
    let mut stream = UnixStream::connect(socket)?;
    stream.set_read_timeout(Some(timeout))?;
    stream.set_write_timeout(Some(timeout))?;
    let mut line = serde_json::to_vec(request)?;
    line.push(b'\n');
    if line.len() > MAX_MESSAGE {
        return Err(IpcError::TooLarge);
    }
    stream.write_all(&line)?;
    Ok(stream)
}

/// Reads one answer line. The server sends nothing unasked, so the buffer
/// never holds more than this line.
fn read_response(stream: &UnixStream) -> Result<Response, IpcError> {
    let mut reader = BufReader::new(std::io::Read::take(stream, MAX_MESSAGE as u64));
    let mut answer = Vec::new();
    reader.read_until(b'\n', &mut answer)?;
    if answer.last() != Some(&b'\n') {
        return Err(IpcError::NoAnswer);
    }
    Ok(serde_json::from_slice(&answer)?)
}

/// Number of descriptors an SSH session passes: stdin, stdout, stderr.
pub const SESSION_FDS: usize = 3;

/// Sends one byte carrying `fds` as `SCM_RIGHTS`.
pub fn send_fds(socket: impl AsFd, fds: &[BorrowedFd<'_>]) -> Result<(), IpcError> {
    let mut space = [MaybeUninit::uninit(); rustix::cmsg_space!(ScmRights(SESSION_FDS))];
    let mut control = SendAncillaryBuffer::new(&mut space);
    if fds.len() > SESSION_FDS || !control.push(SendAncillaryMessage::ScmRights(fds)) {
        return Err(IpcError::TooLarge);
    }
    let sent = rustix::net::sendmsg(
        socket,
        &[IoSlice::new(&[0])],
        &mut control,
        SendFlags::NOSIGNAL,
    )
    .map_err(std::io::Error::from)?;
    if sent == 1 {
        Ok(())
    } else {
        Err(IpcError::NoAnswer)
    }
}

/// Receives the byte sent by [`send_fds`] and exactly [`SESSION_FDS`]
/// descriptors (close-on-exec). Anything else is an error, and descriptors
/// already received are closed. On a non-blocking socket this returns
/// `WouldBlock` like a plain read, so it can run inside `async_io`.
pub fn recv_fds(socket: impl AsFd) -> Result<Vec<OwnedFd>, IpcError> {
    // Room for more than expected, so extra descriptors are seen (and
    // closed) instead of truncated away.
    let mut space = [MaybeUninit::uninit(); rustix::cmsg_space!(ScmRights(SESSION_FDS + 1))];
    let mut control = RecvAncillaryBuffer::new(&mut space);
    let mut byte = [0u8; 1];
    let msg = rustix::net::recvmsg(
        socket,
        &mut [IoSliceMut::new(&mut byte)],
        &mut control,
        RecvFlags::CMSG_CLOEXEC,
    )
    .map_err(std::io::Error::from)?;
    let mut fds = Vec::new();
    for m in control.drain() {
        if let RecvAncillaryMessage::ScmRights(received) = m {
            fds.extend(received);
        }
    }
    if msg.bytes != 1 {
        return Err(IpcError::NoAnswer);
    }
    if msg.flags.contains(ReturnFlags::CTRUNC) || fds.len() != SESSION_FDS {
        return Err(IpcError::Descriptors {
            expected: SESSION_FDS,
            got: fds.len(),
        });
    }
    Ok(fds)
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

    #[test]
    fn session_descriptors_pass_and_must_be_exactly_three() {
        use std::io::{Read, Write as _};
        use std::os::fd::AsFd;

        let (a, b) = UnixStream::pair().unwrap();
        let (mut p0, p1) = UnixStream::pair().unwrap();
        let (q0, q1) = UnixStream::pair().unwrap();
        send_fds(&a, &[p1.as_fd(), q0.as_fd(), q1.as_fd()]).unwrap();
        let fds = recv_fds(&b).unwrap();
        assert_eq!(fds.len(), SESSION_FDS);
        // The first received descriptor is the same socket as p1.
        let mut received = UnixStream::from(fds.into_iter().next().unwrap());
        received.write_all(b"x").unwrap();
        let mut got = [0u8; 1];
        p0.read_exact(&mut got).unwrap();
        assert_eq!(&got, b"x");

        send_fds(&a, &[p1.as_fd(), q0.as_fd()]).unwrap();
        assert!(matches!(
            recv_fds(&b),
            Err(IpcError::Descriptors {
                expected: 3,
                got: 2
            })
        ));
        // A plain byte carries none.
        (&a).write_all(b"\0").unwrap();
        assert!(matches!(
            recv_fds(&b),
            Err(IpcError::Descriptors { got: 0, .. })
        ));
        // More than three are refused by the sender.
        assert!(send_fds(&a, &[p1.as_fd(), q0.as_fd(), q1.as_fd(), p0.as_fd()]).is_err());
        drop(a);
        assert!(matches!(recv_fds(&b), Err(IpcError::NoAnswer)));
    }
}
