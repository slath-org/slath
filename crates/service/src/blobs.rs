// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! The blob store: files addressed by their SHA-256, written atomically.
//!
//! An upload is first written to `blobs/tmp/` while its hash is computed
//! ([`BlobWriter`]), then moved into place under `blobs/<aa>/<sha256>` in the
//! same transaction that records the row using it. The row lock taken by
//! [`slath_db::blobs::upsert_blob`] keeps the collector from deleting the
//! file meanwhile, and the collector removes a file only while holding the
//! lock on a row it is about to delete. A crash can leave a file without a
//! row, never a row without its file; [`Services::collect_blobs`] removes
//! such orphans after the grace period.

use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};
use time::OffsetDateTime;
use tokio::io::AsyncWriteExt;

use crate::Services;
use crate::error::{Result, ServiceError};

/// Bytes of a blob's start kept for type detection.
const HEAD: usize = 64;
/// Temporary files older than this are left over from interrupted uploads.
const TMP_MAX_AGE: std::time::Duration = std::time::Duration::from_secs(3600);
/// Blobs deleted per collector run.
const COLLECT_BATCH: i64 = 500;

/// Content-addressed storage of files.
pub trait BlobStore: Send + Sync {
    /// Starts writing a new blob of at most `max` bytes.
    fn writer(&self, max: u64) -> impl Future<Output = std::io::Result<BlobWriter>> + Send;
    /// Moves a finished blob into place (a no-op if it is already there).
    fn persist(&self, staged: Staged) -> impl Future<Output = std::io::Result<()>> + Send;
    /// Opens a blob for reading.
    fn open(
        &self,
        sha256: &str,
    ) -> impl Future<Output = std::io::Result<Option<tokio::fs::File>>> + Send;
    /// Deletes a blob's file (a no-op if it is gone).
    fn remove(&self, sha256: &str) -> impl Future<Output = std::io::Result<()>> + Send;
}

/// The local filesystem store under `<data_dir>/blobs`.
#[derive(Debug, Clone)]
pub struct LocalBlobStore {
    root: PathBuf,
}

/// Whether `s` is a lowercase hex SHA-256.
#[must_use]
pub fn valid_sha256(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::new(), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

async fn sync_dir(dir: &Path) -> std::io::Result<()> {
    tokio::fs::File::open(dir).await?.sync_all().await
}

impl LocalBlobStore {
    #[must_use]
    pub fn new(data_dir: &Path) -> Self {
        Self {
            root: data_dir.join("blobs"),
        }
    }

    /// Where a blob's file lives. `None` for anything but a SHA-256.
    #[must_use]
    pub fn path(&self, sha256: &str) -> Option<PathBuf> {
        valid_sha256(sha256).then(|| self.root.join(&sha256[..2]).join(sha256))
    }

    /// Every stored blob with its modification time, and leftover temporary
    /// files older than an hour (which are deleted).
    pub async fn scan(&self) -> std::io::Result<Vec<(String, std::time::SystemTime)>> {
        let mut out = Vec::new();
        let mut dirs = match tokio::fs::read_dir(&self.root).await {
            Ok(d) => d,
            Err(e) if e.kind() == ErrorKind::NotFound => return Ok(out),
            Err(e) => return Err(e),
        };
        while let Some(d) = dirs.next_entry().await? {
            let name = d.file_name();
            let Some(name) = name.to_str() else { continue };
            if name == "tmp" {
                let mut files = tokio::fs::read_dir(d.path()).await?;
                while let Some(f) = files.next_entry().await? {
                    let old = f
                        .metadata()
                        .await
                        .and_then(|m| m.modified())
                        .ok()
                        .and_then(|t| t.elapsed().ok())
                        .is_some_and(|age| age > TMP_MAX_AGE);
                    if old {
                        let _ = tokio::fs::remove_file(f.path()).await;
                    }
                }
                continue;
            }
            if name.len() != 2 || !d.file_type().await?.is_dir() {
                continue;
            }
            let mut files = tokio::fs::read_dir(d.path()).await?;
            while let Some(f) = files.next_entry().await? {
                if let Some(sha) = f.file_name().to_str()
                    && valid_sha256(sha)
                    && sha.starts_with(name)
                {
                    let modified = f.metadata().await?.modified()?;
                    out.push((sha.to_owned(), modified));
                }
            }
        }
        Ok(out)
    }
}

/// A blob being written to a temporary file.
pub struct BlobWriter {
    file: tokio::fs::File,
    tmp: Option<PathBuf>,
    hasher: Sha256,
    size: u64,
    max: u64,
    head: Vec<u8>,
}

/// The upload exceeded its size limit.
#[derive(Debug, thiserror::Error)]
pub enum WriteError {
    #[error("too large")]
    TooLarge,
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

impl BlobWriter {
    pub async fn write(&mut self, chunk: &[u8]) -> std::result::Result<(), WriteError> {
        self.size = self.size.saturating_add(chunk.len() as u64);
        if self.size > self.max {
            return Err(WriteError::TooLarge);
        }
        if self.head.len() < HEAD {
            let n = (HEAD - self.head.len()).min(chunk.len());
            self.head.extend_from_slice(&chunk[..n]);
        }
        self.hasher.update(chunk);
        self.file.write_all(chunk).await?;
        Ok(())
    }

    /// Flushes the file and returns the finished blob.
    pub async fn finish(mut self) -> std::io::Result<Staged> {
        self.file.sync_all().await?;
        Ok(Staged {
            sha256: hex(&self.hasher.clone().finalize()),
            size: self.size,
            head: std::mem::take(&mut self.head),
            tmp: self.tmp.take(),
        })
    }
}

impl Drop for BlobWriter {
    fn drop(&mut self) {
        if let Some(tmp) = self.tmp.take() {
            let _ = std::fs::remove_file(tmp);
        }
    }
}

/// A finished blob not yet in place. Dropping it deletes the temporary file.
#[derive(Debug)]
pub struct Staged {
    pub sha256: String,
    pub size: u64,
    /// The first bytes, for type detection.
    pub head: Vec<u8>,
    tmp: Option<PathBuf>,
}

impl Staged {
    #[must_use]
    pub fn size_i64(&self) -> i64 {
        i64::try_from(self.size).unwrap_or(i64::MAX)
    }
}

impl Drop for Staged {
    fn drop(&mut self) {
        if let Some(tmp) = self.tmp.take() {
            let _ = std::fs::remove_file(tmp);
        }
    }
}

impl BlobStore for LocalBlobStore {
    async fn writer(&self, max: u64) -> std::io::Result<BlobWriter> {
        let dir = self.root.join("tmp");
        tokio::fs::create_dir_all(&dir).await?;
        let tmp = dir.join(uuid::Uuid::now_v7().to_string());
        let file = tokio::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)
            .await?;
        Ok(BlobWriter {
            file,
            tmp: Some(tmp),
            hasher: Sha256::new(),
            size: 0,
            max,
            head: Vec::new(),
        })
    }

    async fn persist(&self, mut staged: Staged) -> std::io::Result<()> {
        let dest = self
            .path(&staged.sha256)
            .ok_or_else(|| std::io::Error::other("invalid blob name"))?;
        let Some(tmp) = staged.tmp.take() else {
            return Ok(());
        };
        if tokio::fs::try_exists(&dest).await? {
            let _ = tokio::fs::remove_file(&tmp).await;
            return Ok(());
        }
        let dir = dest.parent().unwrap_or(&self.root);
        tokio::fs::create_dir_all(dir).await?;
        if let Err(e) = tokio::fs::rename(&tmp, &dest).await {
            let _ = tokio::fs::remove_file(&tmp).await;
            return Err(e);
        }
        sync_dir(dir).await
    }

    async fn open(&self, sha256: &str) -> std::io::Result<Option<tokio::fs::File>> {
        let Some(path) = self.path(sha256) else {
            return Ok(None);
        };
        match tokio::fs::File::open(path).await {
            Ok(f) => Ok(Some(f)),
            Err(e) if e.kind() == ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }

    async fn remove(&self, sha256: &str) -> std::io::Result<()> {
        let Some(path) = self.path(sha256) else {
            return Ok(());
        };
        match tokio::fs::remove_file(path).await {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e),
        }
    }
}

/// What the blob collector did.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Collected {
    /// Unused blobs deleted after the grace period.
    pub blobs: usize,
    /// Files without a row (interrupted uploads) deleted.
    pub orphans: usize,
}

impl Services {
    /// Records a staged blob and moves its file into place inside `tx`. The
    /// caller then inserts the row using it and commits.
    pub(crate) async fn store_blob(
        &self,
        tx: &mut sqlx::PgConnection,
        staged: Staged,
    ) -> Result<String> {
        let sha = staged.sha256.clone();
        slath_db::blobs::lock_blob(tx, &sha).await?;
        slath_db::blobs::upsert_blob(tx, &sha, staged.size_i64()).await?;
        self.blobs
            .persist(staged)
            .await
            .map_err(ServiceError::internal)?;
        Ok(sha)
    }

    /// Deletes blobs unused for longer than the grace period, and files no
    /// row refers to that are older than it. Run periodically.
    pub async fn collect_blobs(&self) -> Result<Collected> {
        let grace = time::Duration::try_from(self.config.limits.blob_grace_period.get())
            .unwrap_or(time::Duration::DAY);
        let cutoff = OffsetDateTime::now_utc() - grace;
        let mut done = Collected::default();
        for sha in slath_db::blobs::collectable(&self.pool, cutoff, COLLECT_BATCH).await? {
            let mut tx = self.pool.begin().await.map_err(ServiceError::internal)?;
            if slath_db::blobs::delete_unreferenced(&mut tx, &sha, cutoff).await? {
                self.blobs
                    .remove(&sha)
                    .await
                    .map_err(ServiceError::internal)?;
                tx.commit().await.map_err(ServiceError::internal)?;
                done.blobs += 1;
            }
        }
        let files = self.blobs.scan().await.map_err(ServiceError::internal)?;
        let old = std::time::SystemTime::now()
            .checked_sub(self.config.limits.blob_grace_period.get())
            .unwrap_or(std::time::UNIX_EPOCH);
        for (sha, modified) in files {
            if modified >= old {
                continue;
            }
            // The blob lock serializes with an upload of the same content,
            // which would otherwise reuse the file being deleted.
            let mut tx = self.pool.begin().await.map_err(ServiceError::internal)?;
            if !slath_db::blobs::exists(&mut *tx, &sha).await? {
                slath_db::blobs::lock_blob(&mut tx, &sha).await?;
                if !slath_db::blobs::exists(&mut *tx, &sha).await? {
                    self.blobs
                        .remove(&sha)
                        .await
                        .map_err(ServiceError::internal)?;
                    done.orphans += 1;
                }
            }
            tx.rollback().await.map_err(ServiceError::internal)?;
        }
        Ok(done)
    }
}
