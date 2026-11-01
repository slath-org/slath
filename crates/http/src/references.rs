// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Reference autocomplete: `GET /-/ui/references` for the editor (session
//! cookie; a GET without side effects, so no CSRF token is needed) and
//! `GET /api/v1/repos/{ns}/{repo}/references` (token).

use axum::extract::{Path, Query, State};
use axum::http::{HeaderValue, header};
use axum::response::{IntoResponse, Response};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use slath_core::authz::Subject;
use slath_service::references::Suggestion;
use utoipa::{IntoParams, ToSchema};

use crate::AppState;
use crate::api::ApiUser;
use crate::auth::MaybeSession;
use crate::error::AppError;
use crate::routes::{Access, RouteExt};

/// One suggestion.
#[derive(Serialize, ToSchema)]
pub struct ReferenceJson {
    /// `user`, `team`, `item`, `label`, or `milestone`.
    pub kind: String,
    /// The text to insert, e.g. `@alice`, `#12`, `~"needs review"`.
    pub value: String,
    /// Name or title.
    pub label: String,
    /// Display name, issue state, or description.
    pub detail: Option<String>,
    /// Label color as `rrggbb`.
    pub color: Option<String>,
}

#[derive(Serialize, ToSchema)]
pub struct ReferenceList {
    pub items: Vec<ReferenceJson>,
}

#[derive(Deserialize, IntoParams)]
pub struct ReferenceQuery {
    /// `user`, `team`, `item`, `label`, or `milestone`.
    #[serde(default)]
    pub kind: String,
    /// What was typed so far (with or without the leading `@`, `#`, `~`, `%`).
    #[serde(default)]
    pub q: String,
    /// The issue being edited (its participants rank first).
    pub item: Option<i32>,
}

#[derive(Deserialize)]
struct UiQuery {
    /// `owner/repo`.
    #[serde(default)]
    repo: String,
    #[serde(default)]
    kind: String,
    #[serde(default)]
    q: String,
    item: Option<i32>,
}

fn list(items: Vec<Suggestion>) -> ReferenceList {
    ReferenceList {
        items: items
            .into_iter()
            .map(|s| ReferenceJson {
                kind: s.kind.to_owned(),
                value: s.value,
                label: s.label,
                detail: s.detail,
                color: s.color,
            })
            .collect(),
    }
}

async fn suggest(
    state: &AppState,
    subject: &Subject,
    (ns, repo): (&str, &str),
    k: &str,
    q: &str,
    item: Option<i32>,
) -> Result<Response, AppError> {
    let items = state
        .services
        .reference_suggestions(subject, ns, repo, k, q, item)
        .await?;
    let mut res = Json(list(items)).into_response();
    res.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, no-store"),
    );
    Ok(res)
}

/// Suggestions for completing a reference in this repository's text: at
/// most 10, ranked. Never includes email addresses; 60 requests a minute.
#[utoipa::path(get, path = "/api/v1/repos/{ns}/{repo}/references", tag = "issues", security(("token" = ["repo:read"])), params(("ns" = String, Path), ("repo" = String, Path), ReferenceQuery),
    responses((status = 200, body = ReferenceList), (status = 401, body = crate::api::Problem), (status = 404, body = crate::api::Problem), (status = 422, body = crate::api::Problem), (status = 429, body = crate::api::Problem)))]
pub async fn api_references(
    State(state): State<AppState>,
    ApiUser(s): ApiUser,
    Path((ns, repo)): Path<(String, String)>,
    Query(q): Query<ReferenceQuery>,
) -> Result<Response, AppError> {
    suggest(&state, &s, (&ns, &repo), &q.kind, &q.q, q.item).await
}

async fn ui_references(
    State(state): State<AppState>,
    session: MaybeSession,
    Query(q): Query<UiQuery>,
) -> Result<Response, AppError> {
    let s = session.subject();
    if matches!(s, Subject::Anonymous) {
        return Err(AppError::unauthenticated());
    }
    let (ns, repo) = q.repo.split_once('/').ok_or_else(AppError::not_found)?;
    suggest(&state, &s, (ns, repo), &q.kind, &q.q, q.item).await
}

pub fn ui_router() -> Router<AppState> {
    use axum::routing::get;
    Router::new().route_as("/-/ui/references", Access::Resource, get(ui_references))
}

pub fn api_router() -> Router<AppState> {
    use axum::routing::get;
    Router::new().route_as(
        "/api/v1/repos/{ns}/{repo}/references",
        Access::Resource,
        get(api_references),
    )
}
