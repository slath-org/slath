// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! `/-/design`: the living component catalog (development builds only).

use askama::Template;
use axum::extract::Query;
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

#[derive(Template)]
#[template(path = "design.html")]
struct Catalog {
    ctx: PageCtx,
    count: u32,
    icons: Vec<&'static str>,
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
            icons: icon_names(),
        },
        &DemoFragment { ctx, count },
    )
}

pub fn router() -> axum::Router<AppState> {
    axum::Router::new().route_as("/-/design", Access::Public, axum::routing::get(catalog))
}
