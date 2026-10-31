// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Reading objects through one long-lived `git cat-file --batch-command`
//! process.

use std::fmt;
use std::path::Path;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{ChildStdin, ChildStdout};

use crate::commit::RawCommit;
use crate::error::{GitError, LimitKind};
use crate::runner::{GitChild, Runner};

/// A full SHA-1 object ID: 40 lowercase hex digits.
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ObjectId(String);

impl ObjectId {
    pub fn parse(s: impl AsRef<[u8]>) -> Result<Self, &'static str> {
        let s = s.as_ref();
        if s.len() == 40
            && s.iter()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(b))
        {
            // All bytes are ASCII hex, so this can't fail.
            Ok(Self(String::from_utf8_lossy(s).into_owned()))
        } else {
            Err("object id must be 40 lowercase hex digits")
        }
    }

    fn from_raw(bytes: &[u8]) -> Self {
        use fmt::Write;
        let mut s = String::with_capacity(40);
        for b in bytes {
            let _ = write!(s, "{b:02x}");
        }
        Self(s)
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The all-zero ID Git uses for "no object" in ref updates.
    #[must_use]
    pub fn is_null(&self) -> bool {
        self.0.bytes().all(|b| b == b'0')
    }
}

impl fmt::Display for ObjectId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl fmt::Debug for ObjectId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ObjectId({})", self.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObjectType {
    Commit,
    Tree,
    Blob,
    Tag,
}

impl ObjectType {
    fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "commit" => Self::Commit,
            "tree" => Self::Tree,
            "blob" => Self::Blob,
            "tag" => Self::Tag,
            _ => return None,
        })
    }
}

/// One entry of a tree object.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeEntry {
    /// Octal mode as written by Git, e.g. `100644`, `100755`, `120000`, `40000`, `160000`.
    pub mode: u32,
    /// Entry name; may be non-UTF-8.
    pub name: Vec<u8>,
    pub oid: ObjectId,
}

impl TreeEntry {
    #[must_use]
    pub fn kind(&self) -> EntryKind {
        match self.mode {
            0o040000 => EntryKind::Tree,
            0o120000 => EntryKind::Symlink,
            0o160000 => EntryKind::Submodule,
            0o100755 => EntryKind::Executable,
            _ => EntryKind::File,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    File,
    Executable,
    Symlink,
    Tree,
    Submodule,
}

/// Blob contents, or only the size when the blob is over the requested limit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Blob {
    Data(Vec<u8>),
    TooLarge { size: u64 },
}

/// A `cat-file --batch-command` session on one repository.
///
/// Requests are serialized (`&mut self`). If a request times out, the process
/// is killed and every later call fails with [`GitError::Closed`].
pub struct ObjectReader {
    child: GitChild,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    timeout: Duration,
    max_object: usize,
    broken: bool,
}

impl ObjectReader {
    pub async fn open(runner: &Runner, git_dir: &Path) -> Result<Self, GitError> {
        let timeout = runner.config().read_timeout;
        let max_object = runner.config().max_output_bytes;
        let mut child = runner
            .repo(git_dir)
            .args(["cat-file", "--batch-command"])
            .spawn()
            .await?;
        let (stdin, stdout, stderr) = child.take_stdio()?;
        // Drain stderr so the process never blocks on it.
        tokio::spawn(crate::runner::read_stderr(stderr));
        Ok(Self {
            child,
            stdin,
            stdout: BufReader::new(stdout),
            timeout,
            max_object,
            broken: false,
        })
    }

    /// Returns the type and size of an object, or `None` if it doesn't exist.
    pub async fn info(&mut self, oid: &ObjectId) -> Result<Option<(ObjectType, u64)>, GitError> {
        self.request(format!("info {oid}\n"), None)
            .await
            .map(|r| r.map(|(t, s, _)| (t, s)))
    }

    /// Reads an object's contents if it's at most `limit` bytes.
    pub async fn read(
        &mut self,
        oid: &ObjectId,
        limit: usize,
    ) -> Result<(ObjectType, Vec<u8>), GitError> {
        let (kind, size) = self.info(oid).await?.ok_or(GitError::NotFound)?;
        if size > limit as u64 {
            return Err(GitError::Limit(LimitKind::Output));
        }
        let (_, _, data) = self
            .request(format!("contents {oid}\n"), Some(limit))
            .await?
            .ok_or(GitError::NotFound)?;
        Ok((kind, data))
    }

    async fn read_typed(&mut self, oid: &ObjectId, want: ObjectType) -> Result<Vec<u8>, GitError> {
        let limit = self.max_object;
        let (kind, data) = self.read(oid, limit).await?;
        if kind != want {
            return Err(GitError::NotFound);
        }
        Ok(data)
    }

    /// Reads a commit with all headers preserved (needed for `change-id`).
    pub async fn read_raw_commit(&mut self, oid: &ObjectId) -> Result<RawCommit, GitError> {
        let data = self.read_typed(oid, ObjectType::Commit).await?;
        RawCommit::parse(data).map_err(GitError::Protocol)
    }

    /// Alias of [`ObjectReader::read_raw_commit`]; the parsed form keeps the raw bytes.
    pub async fn read_commit(&mut self, oid: &ObjectId) -> Result<RawCommit, GitError> {
        self.read_raw_commit(oid).await
    }

    pub async fn read_tree(&mut self, oid: &ObjectId) -> Result<Vec<TreeEntry>, GitError> {
        let data = self.read_typed(oid, ObjectType::Tree).await?;
        parse_tree(&data).map_err(GitError::Protocol)
    }

    /// Reads a blob, or reports its size if it's larger than `limit`.
    pub async fn read_blob(&mut self, oid: &ObjectId, limit: usize) -> Result<Blob, GitError> {
        match self.info(oid).await? {
            Some((ObjectType::Blob, size)) if size > limit as u64 => Ok(Blob::TooLarge { size }),
            Some((ObjectType::Blob, _)) => {
                let (_, data) = self.read(oid, limit).await?;
                Ok(Blob::Data(data))
            }
            _ => Err(GitError::NotFound),
        }
    }

    /// Sends one command and parses `<oid> <type> <size>\n[<data>\n]` or
    /// `<oid> missing\n`. `body_limit` is `Some` for `contents` requests.
    async fn request(
        &mut self,
        command: String,
        body_limit: Option<usize>,
    ) -> Result<Option<(ObjectType, u64, Vec<u8>)>, GitError> {
        if self.broken {
            return Err(GitError::Closed);
        }
        let timeout = self.timeout;
        let result = tokio::time::timeout(timeout, self.exchange(command, body_limit)).await;
        match result {
            Ok(Ok(v)) => Ok(v),
            Ok(Err(e)) => {
                self.poison();
                Err(e)
            }
            Err(_) => {
                self.poison();
                Err(GitError::Limit(LimitKind::Timeout))
            }
        }
    }

    fn poison(&mut self) {
        self.broken = true;
        self.child.kill();
    }

    async fn exchange(
        &mut self,
        command: String,
        body_limit: Option<usize>,
    ) -> Result<Option<(ObjectType, u64, Vec<u8>)>, GitError> {
        self.stdin.write_all(command.as_bytes()).await?;
        self.stdin.flush().await?;
        let mut header = Vec::new();
        (&mut self.stdout)
            .take(256)
            .read_until(b'\n', &mut header)
            .await?;
        let line = std::str::from_utf8(&header)
            .ok()
            .and_then(|l| l.strip_suffix('\n'))
            .ok_or(GitError::Protocol("bad cat-file header"))?;
        let mut parts = line.split(' ');
        let (_oid, kind, size) = (parts.next(), parts.next(), parts.next());
        if kind == Some("missing") {
            return Ok(None);
        }
        let kind = kind
            .and_then(ObjectType::parse)
            .ok_or(GitError::Protocol("bad cat-file object type"))?;
        let size: u64 = size
            .and_then(|s| s.parse().ok())
            .ok_or(GitError::Protocol("bad cat-file size"))?;
        let Some(limit) = body_limit else {
            return Ok(Some((kind, size, Vec::new())));
        };
        if size > limit as u64 {
            // Can only happen if the object changed between `info` and
            // `contents`, which Git doesn't allow. Treat as broken.
            return Err(GitError::Limit(LimitKind::Output));
        }
        let len = usize::try_from(size).map_err(|_| GitError::Limit(LimitKind::Output))?;
        let mut data = vec![0u8; len + 1];
        self.stdout.read_exact(&mut data).await?;
        if data.pop() != Some(b'\n') {
            return Err(GitError::Protocol("missing cat-file terminator"));
        }
        Ok(Some((kind, size, data)))
    }
}

fn parse_tree(mut data: &[u8]) -> Result<Vec<TreeEntry>, &'static str> {
    let mut entries = Vec::new();
    while !data.is_empty() {
        let sp = data
            .iter()
            .position(|&b| b == b' ')
            .ok_or("tree: missing space")?;
        let mode_str = std::str::from_utf8(&data[..sp]).map_err(|_| "tree: bad mode")?;
        let mode = u32::from_str_radix(mode_str, 8).map_err(|_| "tree: bad mode")?;
        data = &data[sp + 1..];
        let nul = data
            .iter()
            .position(|&b| b == 0)
            .ok_or("tree: missing NUL")?;
        let name = data[..nul].to_vec();
        data = &data[nul + 1..];
        if data.len() < 20 {
            return Err("tree: truncated object id");
        }
        let oid = ObjectId::from_raw(&data[..20]);
        data = &data[20..];
        entries.push(TreeEntry { mode, name, oid });
    }
    Ok(entries)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn object_ids() {
        assert!(ObjectId::parse("0123456789abcdef0123456789abcdef01234567").is_ok());
        assert!(ObjectId::parse("0123456789ABCDEF0123456789abcdef01234567").is_err());
        assert!(ObjectId::parse("0123").is_err());
        assert!(ObjectId::parse("0123456789abcdef0123456789abcdef0123456\n").is_err());
        assert!(ObjectId::parse("0".repeat(40)).is_ok_and(|o| o.is_null()));
    }

    #[test]
    fn tree_parsing() {
        let mut raw = b"100644 a.txt\0".to_vec();
        raw.extend_from_slice(&[0xab; 20]);
        raw.extend_from_slice(b"40000 dir\0");
        raw.extend_from_slice(&[0x01; 20]);
        let entries = parse_tree(&raw).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].name, b"a.txt");
        assert_eq!(entries[0].oid.as_str(), "ab".repeat(20));
        assert_eq!(entries[1].kind(), EntryKind::Tree);
        assert!(parse_tree(&raw[..raw.len() - 1]).is_err());
    }
}
