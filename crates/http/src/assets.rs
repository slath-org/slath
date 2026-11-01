// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! The UI bundle (`web/dist`), embedded in the binary and served under
//! `/-/assets/<content-hash>/<file>` with an immutable cache policy.

use std::collections::HashMap;

use axum::extract::{Path, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use rust_embed::RustEmbed;
use sha2::{Digest, Sha256};

use crate::AppState;

#[derive(RustEmbed)]
#[folder = "../../web/dist/"]
struct Dist;

/// One embedded file.
#[derive(Debug)]
struct Asset {
    hash: String,
    data: std::borrow::Cow<'static, [u8]>,
    content_type: &'static str,
}

/// Index of the embedded files by name, with their content hashes.
#[derive(Debug, Default)]
pub struct Assets {
    files: HashMap<String, Asset>,
}

fn content_type(name: &str) -> &'static str {
    match name.rsplit_once('.').map(|(_, ext)| ext) {
        Some("js") => "text/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("map" | "json") => "application/json",
        Some("svg") => "image/svg+xml",
        Some("woff2") => "font/woff2",
        Some("png") => "image/png",
        Some("ico") => "image/x-icon",
        Some("txt") => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}

impl Assets {
    /// Loads the embedded bundle.
    #[must_use]
    pub fn embedded() -> Self {
        let mut files = HashMap::new();
        for name in Dist::iter() {
            if let Some(file) = Dist::get(&name) {
                let digest = Sha256::digest(&file.data);
                let hash = digest[..6].iter().map(|b| format!("{b:02x}")).collect();
                files.insert(
                    name.to_string(),
                    Asset {
                        hash,
                        content_type: content_type(&name),
                        data: file.data,
                    },
                );
            }
        }
        Self { files }
    }

    /// The URL of an embedded file, e.g. `/-/assets/1a2b3c4d5e6f/app.js`.
    /// Unknown names give a URL that 404s, so a missing bundle is visible.
    #[must_use]
    pub fn url(&self, name: &str) -> String {
        let hash = self.files.get(name).map_or("missing", |a| a.hash.as_str());
        format!("/-/assets/{hash}/{name}")
    }

    /// True if the bundle was built into this binary.
    #[must_use]
    pub fn is_built(&self) -> bool {
        self.files.contains_key("app.js")
    }

    #[must_use]
    pub fn names(&self) -> Vec<&str> {
        let mut v: Vec<&str> = self.files.keys().map(String::as_str).collect();
        v.sort_unstable();
        v
    }
}

/// `GET /-/assets/{hash}/{*file}`. Only the current hash is served, so a
/// cached URL never returns different content.
pub async fn serve(
    State(state): State<AppState>,
    Path((hash, name)): Path<(String, String)>,
) -> Response {
    match state.assets.files.get(&name) {
        Some(a) if a.hash == hash => (
            [
                (
                    header::CONTENT_TYPE,
                    HeaderValue::from_static(a.content_type),
                ),
                (
                    header::CACHE_CONTROL,
                    HeaderValue::from_static("public, max-age=31536000, immutable"),
                ),
            ],
            a.data.clone(),
        )
            .into_response(),
        _ => StatusCode::NOT_FOUND.into_response(),
    }
}
