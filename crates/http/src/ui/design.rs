// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! `/-/design`: the living component catalog (development builds only).

use askama::Template;
use axum::extract::{Query, State};
use axum::response::Response;
use serde::Deserialize;

use super::{HxRequest, PageCtx, render_hx};
use crate::AppState;
use crate::routes::{Access, RouteExt};

#[derive(Deserialize)]
pub struct DemoQuery {
    #[serde(default)]
    count: u32,
}

/// The icons in the sprite, in allowlist order.
fn icon_names() -> Vec<&'static str> {
    include_str!("../../../../web/src/icons/icons.txt")
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .collect()
}

/// A diff with every kind of file and change, rendered by the same
/// templates as real diffs: changed characters, several hunks, a long
/// line, a whitespace-only file, a rename, a binary file, and a generated
/// one.
const REVIEW_DIFF: &str = concat!(
    "diff --git a/src/config.rs b/src/config.rs\n",
    "index 1111111..2222222 100644\n",
    "--- a/src/config.rs\n",
    "+++ b/src/config.rs\n",
    "@@ -1,6 +1,6 @@ pub struct Config {\n",
    " pub struct Config {\n",
    "-    pub timeout: u32,\n",
    "+    pub timeout_secs: u32,\n",
    "     pub retries: u8,\n",
    "-    pub url: String,\n",
    "+    pub url: Url,\n",
    " }\n",
    "@@ -40,4 +40,5 @@ impl Config {\n",
    "     fn load(path: &Path) -> Result<Self> {\n",
    "-        let text = std::fs::read_to_string(path)?;\n",
    "+        let text = std::fs::read_to_string(path).context(\"reading the configuration\")?;\n",
    "+        // A comment long enough to need scrolling sideways, so the line numbers and markers stay where they are while the code moves.\n",
    "         toml::from_str(&text)\n",
    "     }\n",
    "diff --git a/README.md b/README.md\n",
    "index 3333333..4444444 100644\n",
    "--- a/README.md\n",
    "+++ b/README.md\n",
    "@@ -1,2 +1,2 @@\n",
    "-# Demo  project\n",
    "+# Demo project\n",
    " Read the guide first.\n",
    "diff --git a/docs/old.md b/docs/new.md\n",
    "similarity index 100%\n",
    "rename from docs/old.md\n",
    "rename to docs/new.md\n",
    "diff --git a/assets/logo.png b/assets/logo.png\n",
    "index 5555555..6666666 100644\n",
    "Binary files a/assets/logo.png and b/assets/logo.png differ\n",
);

#[derive(Template)]
#[template(path = "design.html")]
struct Catalog {
    ctx: PageCtx,
    count: u32,
    icons: Vec<&'static str>,
    /// The diff of the code review section.
    diff: super::diff::DiffView,
}

#[derive(Template)]
#[template(path = "design_demo.html")]
struct DemoFragment {
    ctx: PageCtx,
    count: u32,
}

/// `GET /-/design[?count=N]`. The counter is the progressive-enhancement
/// sample: a plain link without JavaScript, an htmx fragment swap with it.
pub async fn catalog(
    State(state): State<AppState>,
    ctx: PageCtx,
    hx: HxRequest,
    Query(q): Query<DemoQuery>,
    Query(dq): Query<super::diff::DiffQuery>,
) -> Response {
    let count = q.count.min(1_000_000);
    let review = super::diff::view(
        REVIEW_DIFF.as_bytes(),
        &state.config.limits,
        &std::collections::HashSet::new(),
        &dq,
        &ctx,
        "/-/design".to_owned(),
    );
    render_hx(
        hx,
        &Catalog {
            ctx: ctx.clone(),
            count,
            icons: icon_names(),
            diff: review,
        },
        &DemoFragment { ctx, count },
    )
}

pub fn router() -> axum::Router<AppState> {
    axum::Router::new().route_as("/-/design", Access::Public, axum::routing::get(catalog))
}
