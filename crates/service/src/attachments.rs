// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Attachments: files uploaded into issue and comment text.
//!
//! Uploading needs the right to comment in the repository; reading needs
//! read access to it, so a private repository's attachments look missing to
//! everyone else. An attachment's ID only works with its own repository.
//! The type is detected from the content: allowlisted images display
//! inline, everything else (SVG, HTML, PDF, text, …) is a download.

use slath_core::authz::{Action, Subject};
use slath_db::blobs::Attachment;

use crate::Services;
use crate::blobs::{BlobStore, BlobWriter, Staged};
use crate::error::{Result, ServiceError};
use crate::repos::RepoAccess;

/// Longest stored file name in bytes.
const MAX_FILENAME: usize = 255;

/// The content type for a file's first bytes: an image type that browsers
/// may display, or `None` for "download only".
#[must_use]
pub fn image_type(head: &[u8]) -> Option<&'static str> {
    if head.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("image/png")
    } else if head.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some("image/jpeg")
    } else if head.starts_with(b"GIF87a") || head.starts_with(b"GIF89a") {
        Some("image/gif")
    } else if head.len() >= 12 && &head[..4] == b"RIFF" && &head[8..12] == b"WEBP" {
        Some("image/webp")
    } else {
        None
    }
}

/// The name to store for an uploaded file: the last path component, with
/// control characters removed, at most 255 bytes.
#[must_use]
pub fn clean_filename(name: &str) -> String {
    let base = name.rsplit(['/', '\\']).next().unwrap_or("");
    let mut out = String::new();
    for c in base.chars().filter(|c| !c.is_control()) {
        if out.len() + c.len_utf8() > MAX_FILENAME {
            break;
        }
        out.push(c);
    }
    let out = out.trim().trim_start_matches('.').trim().to_owned();
    if out.is_empty() {
        "file".to_owned()
    } else {
        out
    }
}

/// The URL path of an attachment.
#[must_use]
pub fn attachment_path(ns: &str, repo: &str, a: &Attachment) -> String {
    let name: String = percent_encode(&a.filename);
    format!("/{ns}/{repo}/attachments/{}/{name}", a.id)
}

/// Percent-encodes everything but unreserved characters.
#[must_use]
pub fn percent_encode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
            out.push(char::from(b));
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

impl Services {
    /// Checks that the subject may upload into a repository, before any of
    /// the upload is read.
    pub async fn attachment_upload_access(
        &self,
        subject: &Subject,
        ns: &str,
        repo: &str,
    ) -> Result<RepoAccess> {
        let a = self
            .issues_access(subject, ns, repo, Action::CommentCreate)
            .await?;
        self.require_confirmed(subject).await?;
        Ok(a)
    }

    /// A writer for an upload of at most `attachment_max_bytes`.
    pub async fn attachment_writer(&self) -> Result<BlobWriter> {
        self.blobs
            .writer(self.config.limits.attachment_max_bytes.0)
            .await
            .map_err(ServiceError::internal)
    }

    /// Stores an uploaded file as an attachment of the repository.
    pub async fn create_attachment(
        &self,
        subject: &Subject,
        a: &RepoAccess,
        filename: &str,
        staged: Staged,
    ) -> Result<Attachment> {
        self.authorize(subject, a, Action::CommentCreate)?;
        let uploader = match subject {
            Subject::User { id, .. } => *id,
            _ => return Err(ServiceError::Unauthenticated),
        };
        if staged.size == 0 {
            return Err(ServiceError::invalid("file", "empty", "The file is empty."));
        }
        let filename = clean_filename(filename);
        let content_type = image_type(&staged.head).unwrap_or("application/octet-stream");
        let size = staged.size_i64();
        let mut tx = self.pool.begin().await.map_err(ServiceError::internal)?;
        let sha = self.store_blob(&mut tx, staged).await?;
        let id = slath_db::blobs::insert_attachment(
            &mut tx,
            a.repo.id,
            uploader,
            &sha,
            &filename,
            content_type,
            size,
        )
        .await?;
        tx.commit().await.map_err(ServiceError::internal)?;
        slath_db::blobs::get_attachment(&self.pool, a.repo.id, id)
            .await?
            .ok_or(ServiceError::NotFound)
    }

    /// An attachment and its file, for a subject who can read the repository.
    pub async fn attachment(
        &self,
        subject: &Subject,
        ns: &str,
        repo: &str,
        id: uuid::Uuid,
    ) -> Result<(RepoAccess, Attachment, tokio::fs::File)> {
        let a = self
            .repo_access(subject, ns, repo, Action::RepoRead)
            .await?;
        let att = slath_db::blobs::get_attachment(&self.pool, a.repo.id, id)
            .await?
            .ok_or(ServiceError::NotFound)?;
        let file = self
            .blobs
            .open(&att.blob_sha256)
            .await
            .map_err(ServiceError::internal)?
            .ok_or_else(|| ServiceError::internal("attachment file missing"))?;
        Ok((a, att, file))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filenames_lose_paths_and_controls() {
        assert_eq!(clean_filename("../../etc/passwd"), "passwd");
        assert_eq!(clean_filename("C:\\x\\a.png"), "a.png");
        assert_eq!(clean_filename("a\u{0}b\nc.txt"), "abc.txt");
        assert_eq!(clean_filename(".."), "file");
        assert_eq!(clean_filename(""), "file");
        assert_eq!(clean_filename(".htaccess"), "htaccess");
        assert!(clean_filename(&"é".repeat(300)).len() <= 255);
    }

    #[test]
    fn only_raster_images_are_inline() {
        assert_eq!(image_type(b"\x89PNG\r\n\x1a\n...."), Some("image/png"));
        assert_eq!(image_type(b"<svg xmlns=..."), None);
        assert_eq!(image_type(b"<!doctype html>"), None);
        assert_eq!(image_type(b"%PDF-1.7"), None);
    }
}
