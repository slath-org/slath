// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! `/-/design`: the living component catalog (development builds only).

use askama::Template;
use axum::extract::Query;
use axum::response::Response;
use serde::Deserialize;

use super::{HxRequest, PageCtx, render_hx};
use crate::AppState;

#[derive(Deserialize)]
pub struct DemoQuery {
    #[serde(default)]
    count: u32,
}

#[derive(Template)]
#[template(path = "design.html")]
struct Catalog {
    ctx: PageCtx,
    count: u32,
}

#[derive(Template)]
#[template(path = "design_demo.html")]
struct DemoFragment {
    ctx: PageCtx,
    count: u32,
}

/// `GET /-/design[?count=N]`. The counter is the progressive-enhancement
/// sample: a plain link without JavaScript, an htmx fragment swap with it.
pub async fn catalog(ctx: PageCtx, hx: HxRequest, Query(q): Query<DemoQuery>) -> Response {
    let count = q.count.min(1_000_000);
    render_hx(
        hx,
        &Catalog {
            ctx: ctx.clone(),
            count,
        },
        &DemoFragment { ctx, count },
    )
}

pub fn router() -> axum::Router<AppState> {
    axum::Router::new().route("/-/design", axum::routing::get(catalog))
}
