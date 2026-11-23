// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! The documentation of this version, embedded in the binary and served
//! at `/-/docs/` to everyone, so an instance without internet access has
//! all of it. Only the published folders are embedded (`docs/user`,
//! `docs/admin`, `docs/cli`, `docs/dev`, the OpenAPI document, and the
//! theme); planning documents never are.

use std::sync::OnceLock;

use axum::Router;
use axum::extract::Path;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Redirect, Response};
use rust_embed::RustEmbed;
use slath_markup::docs::{Options, Site, Source, Theme};

use crate::AppState;
use crate::routes::{Access, RouteExt};

#[derive(RustEmbed)]
#[folder = "../../docs/"]
#[include = "user/*.md"]
#[include = "admin/*.md"]
#[include = "cli/*.md"]
#[include = "cli/*.json"]
#[include = "dev/*.md"]
#[include = "api/openapi.json"]
#[include = "api/*.schema.json"]
#[include = "theme/page.html"]
#[include = "theme/docs.css"]
#[include = "theme/docs.js"]
struct DocSources;

/// The documentation's sources, as embedded.
#[must_use]
pub fn sources() -> Vec<Source> {
    let mut v: Vec<Source> = DocSources::iter()
        .filter(|p| !p.starts_with("theme/"))
        .filter_map(|p| {
            DocSources::get(&p).map(|f| Source {
                path: p.to_string(),
                bytes: f.data.into_owned(),
            })
        })
        .collect();
    v.sort_by(|a, b| a.path.cmp(&b.path));
    v
}

/// The theme, with the Weave tokens, the interface font, and the logos.
#[must_use]
pub fn theme() -> Theme {
    let file = |name: &str| {
        DocSources::get(&format!("theme/{name}"))
            .map(|f| f.data.into_owned())
            .unwrap_or_default()
    };
    Theme {
        page: String::from_utf8_lossy(&file("page.html")).into_owned(),
        files: vec![
            ("docs.css".to_owned(), file("docs.css")),
            ("docs.js".to_owned(), file("docs.js")),
            (
                "tokens.css".to_owned(),
                include_bytes!("../../../web/src/tokens.css").to_vec(),
            ),
            (
                "SourceSans3-Upright.woff2".to_owned(),
                include_bytes!("../../../web/src/fonts/SourceSans3-Upright.woff2").to_vec(),
            ),
            (
                "logo-light.svg".to_owned(),
                include_bytes!("../../../web/src/brand/logo-light.svg").to_vec(),
            ),
            (
                "logo-dark.svg".to_owned(),
                include_bytes!("../../../web/src/brand/logo-dark.svg").to_vec(),
            ),
        ],
    }
}

/// Builds the site for `version` (`published`: for slath.org, with the
/// version switcher).
pub fn build(version: &str, published: bool) -> Result<Site, Vec<String>> {
    slath_markup::docs::build(
        &sources(),
        &theme(),
        &Options {
            version: version.to_owned(),
            published,
        },
    )
}

/// This binary's documentation, built on first use.
pub fn site() -> &'static Site {
    static SITE: OnceLock<Site> = OnceLock::new();
    SITE.get_or_init(|| {
        build(env!("CARGO_PKG_VERSION"), false).unwrap_or_else(|errors| {
            // The tests build it too, so this can't happen in a release.
            tracing::error!(?errors, "building the documentation failed");
            Site::default()
        })
    })
}

fn content_type(path: &str) -> &'static str {
    match path.rsplit('.').next() {
        Some("html") => "text/html; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("js") => "text/javascript; charset=utf-8",
        Some("json") => "application/json",
        Some("svg") => "image/svg+xml",
        Some("woff2") => "font/woff2",
        _ => "application/octet-stream",
    }
}

fn serve(path: &str) -> Response {
    let path = if path.is_empty() || path.ends_with('/') {
        format!("{path}index.html")
    } else {
        path.to_owned()
    };
    let Some(bytes) = site().files.get(&path) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let mut res = bytes.clone().into_response();
    let h = res.headers_mut();
    h.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static(content_type(&path)),
    );
    h.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("public, max-age=3600"),
    );
    res
}

async fn index() -> Response {
    serve("")
}

async fn page(Path(path): Path<String>) -> Response {
    serve(&path)
}

pub fn router() -> Router<AppState> {
    use axum::routing::get;
    Router::new()
        .route_as(
            "/-/docs",
            Access::Public,
            get(|| async { Redirect::permanent("/-/docs/") }),
        )
        .route_as("/-/docs/", Access::Public, get(index))
        .route_as("/-/docs/{*path}", Access::Public, get(page))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The real documentation builds: no broken link or anchor anywhere,
    /// so a broken one fails `just check`.
    #[test]
    fn conf_docs_01_the_documentation_builds_without_broken_links() {
        let site = build("0.0.0-test", false).unwrap_or_else(|e| panic!("{}", e.join("\n")));
        for page in [
            "index.html",
            "user/merge-requests.html",
            "admin/install.html",
            "cli/index.html",
            "dev/setup.html",
            "api/index.html",
        ] {
            assert!(site.files.contains_key(page), "{page}");
        }
        assert!(
            !sources().iter().any(|s| s.path.starts_with("spec/")
                || s.path.starts_with("plan/")
                || s.path.contains("DECISIONS")),
            "planning documents are never embedded"
        );
        // Nothing outside the site: no absolute URL in a resource position.
        for (path, bytes) in &site.files {
            if path.ends_with(".html") {
                let html = String::from_utf8_lossy(bytes);
                for attr in ["src=\"http", "href=\"//", "url(http"] {
                    assert!(!html.contains(attr), "{path}: {attr}");
                }
                assert!(
                    !html.contains("<link rel=\"stylesheet\" href=\"http"),
                    "{path}"
                );
            }
        }
        assert!(build("0.0.0-test", true).is_ok());
    }

    /// Every help link in the templates and handlers goes to a page of
    /// the documentation that exists, and to a heading on it that does.
    #[test]
    fn conf_docs_01_every_help_link_resolves() {
        let site = build("0.0.0-test", false).unwrap_or_else(|e| panic!("{}", e.join("\n")));
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let mut found = 0;
        let mut stack = vec![root.join("templates"), root.join("src")];
        while let Some(dir) = stack.pop() {
            for e in std::fs::read_dir(&dir).unwrap().flatten() {
                let path = e.path();
                if path.is_dir() {
                    stack.push(path);
                    continue;
                }
                let text = std::fs::read_to_string(&path).unwrap_or_default();
                for (i, _) in text.match_indices("\"/-/docs/") {
                    let link = &text[i + 1..];
                    let link = &link[..link.find('"').unwrap_or(link.len())];
                    if link.contains('{') {
                        continue;
                    }
                    let rest = &link["/-/docs/".len()..];
                    let (page, anchor) = rest.split_once('#').unwrap_or((rest, ""));
                    let page = if page.is_empty() || page.ends_with('/') {
                        format!("{page}index.html")
                    } else {
                        page.to_owned()
                    };
                    assert!(site.files.contains_key(&page), "{}: {link}", path.display());
                    if !anchor.is_empty() {
                        assert!(
                            site.anchors.get(&page).is_some_and(|a| a.contains(anchor)),
                            "{}: {link} has no such heading",
                            path.display()
                        );
                    }
                    found += 1;
                }
            }
        }
        assert!(found >= 6, "only {found} help links found");
    }
}
