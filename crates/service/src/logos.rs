// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! The instance logo: site administrators replace the bundled Slath logo
//! with their own, one image for the light theme and one for the dark.
//!
//! An SVG is stored as uploaded once `slath_markup::svg::check_logo`
//! accepts it (drawing only: no scripts, handlers, styles sheets, or
//! references outside the file). A PNG or WebP is decoded with limits,
//! scaled to fit [`LOGO_MAX_SIDE`] pixels, and encoded again as PNG, so
//! only pixels survive. Pages read the logos from a cache that changes
//! with them.

use std::io::Cursor;
use std::sync::{Arc, RwLock};

use image::{ImageFormat, ImageReader, imageops};
use slath_core::authz::Subject;
use slath_db::blobs::InstanceLogo;

use crate::Services;
use crate::blobs::BlobStore;
use crate::error::{Result, ServiceError};

/// The longest side of a stored PNG logo.
pub const LOGO_MAX_SIDE: u32 = 512;
/// Largest accepted source dimension.
const MAX_SOURCE_DIM: u32 = 8192;
/// Memory the decoder may allocate.
const MAX_DECODE_ALLOC: u64 = 256 * 1024 * 1024;

/// The uploaded logos, shared by every page; `None` until first read.
#[derive(Debug, Default)]
pub struct LogoCache {
    inner: RwLock<Option<Arc<Vec<InstanceLogo>>>>,
}

fn invalid(field: &str, message: impl Into<String>) -> ServiceError {
    ServiceError::invalid(field, "invalid_logo", message)
}

/// A checked logo: the bytes to store and their content type.
fn prepare(field: &str, data: &[u8]) -> Result<(Vec<u8>, &'static str)> {
    let head = data.iter().take(512).copied().collect::<Vec<u8>>();
    let looks_like_text = String::from_utf8_lossy(&head).trim_start().starts_with('<');
    if looks_like_text {
        slath_markup::svg::check_logo(data).map_err(|why| invalid(field, why))?;
        return Ok((data.to_vec(), "image/svg+xml"));
    }
    let not_an_image = || invalid(field, "Upload an SVG, PNG, or WebP image.");
    let mut reader = ImageReader::new(Cursor::new(data))
        .with_guessed_format()
        .map_err(|_| not_an_image())?;
    if !matches!(reader.format(), Some(ImageFormat::Png | ImageFormat::WebP)) {
        return Err(not_an_image());
    }
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_SOURCE_DIM);
    limits.max_image_height = Some(MAX_SOURCE_DIM);
    limits.max_alloc = Some(MAX_DECODE_ALLOC);
    reader.limits(limits);
    let img = reader.decode().map_err(|_| not_an_image())?.to_rgba8();
    let (w, h) = img.dimensions();
    if w == 0 || h == 0 {
        return Err(not_an_image());
    }
    let scale = f64::from(LOGO_MAX_SIDE) / f64::from(w.max(h));
    let img = if scale < 1.0 {
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let (nw, nh) = (
            ((f64::from(w) * scale).round() as u32).max(1),
            ((f64::from(h) * scale).round() as u32).max(1),
        );
        imageops::resize(&img, nw, nh, imageops::FilterType::Lanczos3)
    } else {
        img
    };
    let mut out = Vec::new();
    img.write_to(&mut Cursor::new(&mut out), ImageFormat::Png)
        .map_err(ServiceError::internal)?;
    Ok((out, "image/png"))
}

impl Services {
    /// The uploaded logos (empty: the bundled logo everywhere).
    pub async fn instance_logos(&self) -> Arc<Vec<InstanceLogo>> {
        if let Some(l) = self
            .logo_cache
            .inner
            .read()
            .ok()
            .and_then(|g| g.as_ref().map(Arc::clone))
        {
            return l;
        }
        let logos = Arc::new(
            slath_db::blobs::instance_logos(&self.pool)
                .await
                .unwrap_or_else(|e| {
                    tracing::warn!(error = %e, "reading the instance logos failed");
                    Vec::new()
                }),
        );
        if let Ok(mut g) = self.logo_cache.inner.write() {
            *g = Some(Arc::clone(&logos));
        }
        logos
    }

    /// The uploaded logos as last read, without waiting (error pages);
    /// `None` before the first read.
    #[must_use]
    pub fn cached_instance_logos(&self) -> Option<Arc<Vec<InstanceLogo>>> {
        self.logo_cache
            .inner
            .read()
            .ok()
            .and_then(|g| g.as_ref().map(Arc::clone))
    }

    /// Reads the logos again after a change, so the next page has them.
    async fn reload_logos(&self) {
        if let Ok(mut g) = self.logo_cache.inner.write() {
            *g = None;
        }
        self.instance_logos().await;
    }

    /// Replaces the logo of each theme given (`light`, `dark`) with an
    /// uploaded image. Site administrators only.
    pub async fn set_instance_logos(
        &self,
        subject: &Subject,
        uploads: &[(&str, Vec<u8>)],
    ) -> Result<()> {
        self.require_site_admin(subject)?;
        let Subject::User { id: by, .. } = subject else {
            return Err(ServiceError::NotFound);
        };
        let max = self.config.limits.logo_max_bytes.0;
        let mut prepared = Vec::new();
        for (theme, data) in uploads {
            let field = format!("logo_{theme}");
            if !matches!(*theme, "light" | "dark") {
                return Err(invalid(&field, "Choose the light or the dark logo."));
            }
            if data.len() as u64 > max {
                return Err(ServiceError::invalid(
                    &field,
                    "too_large",
                    format!("Use an image of at most {} KiB.", max / 1024),
                ));
            }
            let data = data.clone();
            let f = field.clone();
            let (bytes, content_type) = tokio::task::spawn_blocking(move || prepare(&f, &data))
                .await
                .map_err(ServiceError::internal)??;
            prepared.push((*theme, bytes, content_type));
        }
        if prepared.is_empty() {
            return Err(invalid(
                "logo_light",
                "Choose an image for at least one theme.",
            ));
        }
        let mut tx = self.pool.begin().await.map_err(ServiceError::internal)?;
        for (theme, bytes, content_type) in &prepared {
            let mut w = self
                .blobs
                .writer(bytes.len() as u64)
                .await
                .map_err(ServiceError::internal)?;
            w.write(bytes).await.map_err(ServiceError::internal)?;
            let staged = w.finish().await.map_err(ServiceError::internal)?;
            let sha = self.store_blob(&mut tx, staged).await?;
            slath_db::blobs::set_instance_logo(&mut tx, theme, &sha, content_type, *by).await?;
        }
        tx.commit().await.map_err(ServiceError::internal)?;
        self.reload_logos().await;
        let themes: Vec<&str> = prepared.iter().map(|p| p.0).collect();
        self.audit(
            subject,
            "admin.logo.set",
            crate::audit::Target::new("instance", None),
            serde_json::json!({"themes": themes}),
        )
        .await;
        Ok(())
    }

    /// Goes back to the bundled logo. Site administrators only.
    pub async fn reset_instance_logos(&self, subject: &Subject) -> Result<()> {
        self.require_site_admin(subject)?;
        slath_db::blobs::reset_instance_logos(&self.pool).await?;
        self.reload_logos().await;
        self.audit(
            subject,
            "admin.logo.reset",
            crate::audit::Target::new("instance", None),
            serde_json::json!({}),
        )
        .await;
        Ok(())
    }

    /// An uploaded logo's file and content type (anyone may load it: every
    /// page shows it).
    pub async fn logo_file(&self, sha256: &str) -> Result<(tokio::fs::File, String)> {
        if !crate::blobs::valid_sha256(sha256) {
            return Err(ServiceError::NotFound);
        }
        let Some(content_type) = slath_db::blobs::logo_type(&self.pool, sha256).await? else {
            return Err(ServiceError::NotFound);
        };
        let file = self
            .blobs
            .open(sha256)
            .await
            .map_err(ServiceError::internal)?
            .ok_or(ServiceError::NotFound)?;
        Ok((file, content_type))
    }
}

#[cfg(test)]
mod tests {
    use super::prepare;

    fn png(w: u32, h: u32) -> Vec<u8> {
        let img = image::RgbaImage::from_pixel(w, h, image::Rgba([10, 20, 30, 255]));
        let mut out = Vec::new();
        img.write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
            .unwrap();
        out
    }

    #[test]
    fn pngs_are_reencoded_and_fit_the_size() {
        let mut big = png(2048, 1024);
        // A trailing chunk of junk, as a polyglot would carry: gone after.
        big.extend_from_slice(b"<script>alert(1)</script>");
        let (out, ct) = prepare("logo_light", &big).unwrap();
        assert_eq!(ct, "image/png");
        assert!(!String::from_utf8_lossy(&out).contains("<script>"));
        let img = image::load_from_memory(&out).unwrap();
        assert_eq!((img.width(), img.height()), (512, 256));
        let (small, _) = prepare("logo_light", &png(24, 24)).unwrap();
        assert_eq!(image::load_from_memory(&small).unwrap().width(), 24);
    }

    #[test]
    fn svgs_are_checked_and_other_files_refused() {
        let ok =
            br#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 1 1"><path d="M0 0"/></svg>"#;
        assert_eq!(prepare("logo_dark", ok).unwrap().1, "image/svg+xml");
        let bad = br#"<svg xmlns="http://www.w3.org/2000/svg" onload="alert(1)"/>"#;
        assert!(prepare("logo_dark", bad).is_err());
        assert!(prepare("logo_dark", b"GIF89a....").is_err());
        assert!(prepare("logo_dark", b"").is_err());
    }
}
