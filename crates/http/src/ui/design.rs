// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! `/-/design`: the living component catalog (development builds only).

use askama::Template;
use axum::extract::{Path, Query, State};
use axum::response::Response;
use serde::Deserialize;

use super::{HxRequest, PageCtx, render, render_hx};
use crate::AppState;
use crate::error::AppError;
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

/// The layout family: stack, cluster, page, section, subsection, frame.
#[derive(Template)]
#[template(path = "design/layout.html")]
struct LayoutFamily {
    ctx: PageCtx,
}

/// The messages family: notices, summaries, empty and busy states.
#[derive(Template)]
#[template(path = "design/messages.html")]
struct MessagesFamily {
    ctx: PageCtx,
}

/// The forms family: fields, questions, groups, buttons, danger.
#[derive(Template)]
#[template(path = "design/forms.html")]
struct FormsFamily {
    ctx: PageCtx,
}

/// The disclosure family: disclosures, menus, the reply box.
#[derive(Template)]
#[template(path = "design/disclosure.html")]
struct DisclosureFamily {
    ctx: PageCtx,
}

/// The lists family: list rows, tables, facts, the pager.
#[derive(Template)]
#[template(path = "design/lists.html")]
struct ListsFamily {
    ctx: PageCtx,
}

/// The people family: avatars, account names, Git identities.
#[derive(Template)]
#[template(path = "design/people.html")]
struct PeopleFamily {
    ctx: PageCtx,
    few: Vec<String>,
    many: Vec<String>,
}

/// The basic controls family: links, tooltips, times, hashes, paths.
#[derive(Template)]
#[template(path = "design/controls.html")]
struct ControlsFamily {
    ctx: PageCtx,
    recent: super::Time,
    soon: super::Time,
    older: super::Time,
    oid: String,
}

/// The timeline and status family: status lines, the readiness card,
/// the timeline, threads, badges.
#[derive(Template)]
#[template(path = "design/review.html")]
struct ReviewFamily {
    ctx: PageCtx,
    recent: super::Time,
    older: super::Time,
}

/// `GET /-/design/{family}`: one family of components, each with its
/// contract and examples.
async fn family(ctx: PageCtx, Path(name): Path<String>) -> Result<Response, AppError> {
    match name.as_str() {
        "layout" => Ok(render(&LayoutFamily { ctx })),
        "messages" => Ok(render(&MessagesFamily { ctx })),
        "forms" => Ok(render(&FormsFamily { ctx })),
        "disclosure" => Ok(render(&DisclosureFamily { ctx })),
        "lists" => Ok(render(&ListsFamily { ctx })),
        "controls" => {
            let now = time::OffsetDateTime::now_utc();
            Ok(render(&ControlsFamily {
                ctx,
                recent: (now - time::Duration::hours(3)).into(),
                soon: (now + time::Duration::days(2) + time::Duration::hours(1)).into(),
                older: (now - time::Duration::days(40)).into(),
                oid: "a1e62f5c7d0b9a8e3f41c2d6b7e8f90a1b2c3d4e".to_owned(),
            }))
        }
        "review" => {
            let now = time::OffsetDateTime::now_utc();
            Ok(render(&ReviewFamily {
                ctx,
                recent: (now - time::Duration::hours(2)).into(),
                older: (now - time::Duration::days(12)).into(),
            }))
        }
        "people" => {
            let names = |n: &[&str]| n.iter().map(|s| (*s).to_owned()).collect();
            Ok(render(&PeopleFamily {
                ctx,
                few: names(&["alice", "bob"]),
                many: names(&["alice", "bob", "carol", "dee", "eve"]),
            }))
        }
        _ => Err(AppError::not_found()),
    }
}

pub fn router() -> axum::Router<AppState> {
    axum::Router::new()
        .route_as("/-/design", Access::Public, axum::routing::get(catalog))
        .route_as(
            "/-/design/{family}",
            Access::Public,
            axum::routing::get(family),
        )
}
