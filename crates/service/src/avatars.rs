// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Uploaded avatars for users and organizations.
//!
//! An upload (PNG, JPEG, or WebP) is decoded with size limits, cropped to a
//! square, scaled to [`AVATAR_SIZE`] pixels, and encoded again as PNG. Only
//! pixels survive: EXIF, GPS, color profiles, and comments of the original
//! are dropped. Without an upload, pages show a generated identicon.

use std::io::Cursor;

use image::{ImageFormat, ImageReader, imageops};
use slath_core::authz::{Scope, Subject};
use slath_core::ids::OrgId;

use crate::Services;
use crate::blobs::BlobStore;
use crate::error::{Result, ServiceError};

/// Width and height of stored avatars.
pub const AVATAR_SIZE: u32 = 256;
/// Largest accepted source dimension.
const MAX_SOURCE_DIM: u32 = 8192;
/// Memory the decoder may allocate.
const MAX_DECODE_ALLOC: u64 = 256 * 1024 * 1024;

fn not_an_image() -> ServiceError {
    ServiceError::invalid(
        "avatar",
        "invalid_image",
        "Upload a PNG, JPEG, or WebP image.",
    )
}

/// Decodes an image and re-encodes it as a square PNG without metadata.
pub fn reencode(data: &[u8]) -> Result<Vec<u8>> {
    let mut reader = ImageReader::new(Cursor::new(data))
        .with_guessed_format()
        .map_err(|_| not_an_image())?;
    if !matches!(
        reader.format(),
        Some(ImageFormat::Png | ImageFormat::Jpeg | ImageFormat::WebP)
    ) {
        return Err(not_an_image());
    }
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_SOURCE_DIM);
    limits.max_image_height = Some(MAX_SOURCE_DIM);
    limits.max_alloc = Some(MAX_DECODE_ALLOC);
    reader.limits(limits);
    let img = reader.decode().map_err(|_| not_an_image())?;
    let (w, h) = (img.width(), img.height());
    if w == 0 || h == 0 {
        return Err(not_an_image());
    }
    let side = w.min(h);
    let square = img.crop_imm((w - side) / 2, (h - side) / 2, side, side);
    let scaled = imageops::resize(
        &square.to_rgba8(),
        AVATAR_SIZE,
        AVATAR_SIZE,
        imageops::FilterType::Lanczos3,
    );
    let mut out = Vec::new();
    scaled
        .write_to(&mut Cursor::new(&mut out), ImageFormat::Png)
        .map_err(ServiceError::internal)?;
    Ok(out)
}

/// Whose avatar is changed.
#[derive(Debug, Clone, Copy)]
pub enum AvatarOwner {
    /// The signed-in user.
    Me,
    Org(OrgId),
    /// A bot the subject manages.
    Bot(slath_core::ids::UserId),
}

impl Services {
    /// Replaces an avatar with an uploaded image (`None` removes it).
    pub async fn set_avatar(
        &self,
        subject: &Subject,
        owner: AvatarOwner,
        upload: Option<Vec<u8>>,
    ) -> Result<()> {
        let user = crate::tokens::user_with_scope(subject, Scope::UserWrite)?;
        match owner {
            AvatarOwner::Me => {}
            AvatarOwner::Org(org) => {
                self.require_org_owner(subject, org).await?;
            }
            AvatarOwner::Bot(bot) => {
                let name = slath_db::users::get(&self.pool, bot)
                    .await?
                    .ok_or(ServiceError::NotFound)?
                    .name;
                self.managed_bot(subject, &name, Scope::UserWrite).await?;
            }
        }
        let png = match upload {
            Some(data) => {
                if data.len() as u64 > self.config.limits.avatar_max_bytes.0 {
                    return Err(ServiceError::invalid(
                        "avatar",
                        "too_large",
                        format!(
                            "Use an image of at most {} MiB.",
                            self.config.limits.avatar_max_bytes.0 / (1024 * 1024)
                        ),
                    ));
                }
                Some(
                    tokio::task::spawn_blocking(move || reencode(&data))
                        .await
                        .map_err(ServiceError::internal)??,
                )
            }
            None => None,
        };
        let mut tx = self.pool.begin().await.map_err(ServiceError::internal)?;
        let sha = match png {
            Some(png) => {
                let mut w = self
                    .blobs
                    .writer(png.len() as u64)
                    .await
                    .map_err(ServiceError::internal)?;
                w.write(&png).await.map_err(ServiceError::internal)?;
                let staged = w.finish().await.map_err(ServiceError::internal)?;
                Some(self.store_blob(&mut tx, staged).await?)
            }
            None => None,
        };
        match owner {
            AvatarOwner::Me => {
                slath_db::blobs::set_user_avatar(&mut tx, user, sha.as_deref()).await?
            }
            AvatarOwner::Org(org) => {
                slath_db::blobs::set_org_avatar(&mut tx, org, sha.as_deref()).await?;
            }
            AvatarOwner::Bot(bot) => {
                slath_db::blobs::set_user_avatar(&mut tx, bot, sha.as_deref()).await?;
            }
        }
        tx.commit().await.map_err(ServiceError::internal)?;
        Ok(())
    }

    /// The avatar URL of a user or organization: the uploaded image, or the
    /// identicon.
    pub async fn avatar_url(&self, name: &str) -> String {
        match slath_db::blobs::namespace_avatar(&self.pool, name).await {
            Ok(Some(sha)) => format!("/-/avatars/{sha}.png"),
            _ => format!("/-/avatars/{name}.svg"),
        }
    }

    /// An uploaded avatar's file. Only blobs in use as an avatar are served
    /// here; attachments are never reachable through this path.
    pub async fn avatar_file(&self, sha256: &str) -> Result<tokio::fs::File> {
        if !crate::blobs::valid_sha256(sha256)
            || !slath_db::blobs::is_avatar(&self.pool, sha256).await?
        {
            return Err(ServiceError::NotFound);
        }
        self.blobs
            .open(sha256)
            .await
            .map_err(ServiceError::internal)?
            .ok_or(ServiceError::NotFound)
    }
}
