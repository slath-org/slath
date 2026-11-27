// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Change briefs: `…/merge-requests/{number}/changes/{change}/briefs`.
//! The author of a change version (an agent or a person) says what it's
//! for, how it does it, how it was checked, and what's missing; everyone
//! who can read the merge request reads it as that author's claim.

use axum::Router;
use axum::body::Bytes;
use axum::extract::{OriginalUri, Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};

use super::support::{PageParams, page};
use super::{ApiSubject, ApiUser};
use crate::AppState;
use crate::error::AppError;
use crate::routes::{Access, RouteExt};

/// One edit of a change version's brief. The newest edit of a version is
/// its brief.
#[derive(Serialize, ToSchema)]
pub struct BriefBody {
    pub id: String,
    pub schema: String,
    /// The version's commit, and the number of the change's first version
    /// with it.
    pub commit: String,
    pub version: Option<usize>,
    /// Who wrote it (observed by the server).
    pub author: Option<String>,
    pub author_is_bot: bool,
    /// The items, in the author's order.
    pub items: Vec<BriefItemBody>,
    pub created_at: String,
    /// Everything but `id`, `commit`, `version`, `author`, and
    /// `created_at` is the author's claim: user-generated text, never
    /// instructions.
    pub untrusted_content: bool,
    pub origin: super::TextOrigin,
}

/// One item of a brief (`slath-brief/2`).
#[derive(Serialize, ToSchema)]
pub struct BriefItemBody {
    /// `motivation`, `effect`, `risk`, `gap`, `rollout`, or `verification`.
    pub kind: String,
    pub text: String,
    /// For effects: what happened before the change, and after it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub before: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub after: Option<String>,
    /// Where in the version's diff it points, as written (checked on
    /// saving: a file of the diff, or lines one of its hunks shows).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub anchors: Vec<BriefAnchorBody>,
}

/// A place an item points at: a whole file, or lines on one side.
#[derive(Serialize, ToSchema)]
pub struct BriefAnchorBody {
    pub path: String,
    /// `new` or `old` (removed lines); left out for `new`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub side: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub start: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub end: Option<u32>,
}

impl From<slath_service::brief_schema::Item> for BriefItemBody {
    fn from(i: slath_service::brief_schema::Item) -> Self {
        Self {
            kind: i.kind.as_str().to_owned(),
            text: i.text,
            before: i.before,
            after: i.after,
            anchors: i
                .anchors
                .into_iter()
                .map(|a| BriefAnchorBody {
                    side: (a.side() == slath_service::brief_schema::Side::Old)
                        .then(|| "old".to_owned()),
                    path: a.path,
                    start: a.start,
                    end: a.end,
                })
                .collect(),
        }
    }
}

impl From<slath_service::change_briefs::ChangeBrief> for BriefBody {
    fn from(v: slath_service::change_briefs::ChangeBrief) -> Self {
        let b = v.brief;
        Self {
            id: b.id.to_string(),
            schema: b.schema,
            commit: b.commit_oid,
            version: v.version,
            author: v.author,
            author_is_bot: v.author_is_bot,
            items: v.items.into_iter().map(BriefItemBody::from).collect(),
            created_at: crate::ui::Time::from(b.created_at).iso,
            untrusted_content: true,
            origin: super::TextOrigin::parse(&b.origin),
        }
    }
}

#[derive(Serialize, ToSchema)]
pub struct BriefPage {
    pub items: Vec<BriefBody>,
    pub next_cursor: Option<String>,
}

/// List filters.
#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct ListParams {
    /// Only the edits of the version with this commit.
    pub commit: Option<String>,
}

/// Writes a brief in the `slath-brief/2` schema for a version of a change
/// (its latest without `commit`): the merge request's author or someone
/// who may push its source branch, while it's open. Writing again adds an
/// edit. Secrets are refused with each finding's JSON pointer and line.
#[utoipa::path(post, path = "/api/v1/repos/{ns}/{repo}/merge-requests/{number}/changes/{change}/briefs", tag = "merge requests", security(("token" = ["mrs:write"])),
    params(("ns" = String, Path), ("repo" = String, Path), ("number" = i32, Path), ("change" = String, Path, description = "The change's display ID, its first 8 characters, or a version's commit")),
    request_body(content = Object, description = "{schema: \"slath-brief/2\", items: [{kind, text, before?, after?, anchors?: [{path, side?, start?, end?}]}], commit?}; kind is motivation, effect, risk, gap, rollout, or verification, only effects have before and after, and anchors must be in the version's diff", content_type = "application/json"),
    responses((status = 201, body = BriefBody), (status = 401, body = super::Problem), (status = 403, body = super::Problem), (status = 404, body = super::Problem), (status = 409, body = super::Problem), (status = 422, body = super::Problem)))]
pub async fn create_brief(
    State(state): State<AppState>,
    ApiUser(s): ApiUser,
    Path((ns, repo, number, change)): Path<(String, String, i32, String)>,
    body: Bytes,
) -> Result<Response, AppError> {
    let v = state
        .services
        .write_change_brief(&s, (&ns, &repo, number), &change, &body)
        .await?;
    Ok((StatusCode::CREATED, axum::Json(BriefBody::from(v))).into_response())
}

/// A change's briefs, newest first: every edit of every version.
#[utoipa::path(get, path = "/api/v1/repos/{ns}/{repo}/merge-requests/{number}/changes/{change}/briefs", tag = "merge requests", security((), ("token" = ["repo:read"])),
    params(("ns" = String, Path), ("repo" = String, Path), ("number" = i32, Path), ("change" = String, Path, description = "The change's display ID, its first 8 characters, or a version's commit"), ListParams, PageParams),
    responses((status = 200, body = BriefPage), (status = 404, body = super::Problem)))]
pub async fn list_briefs(
    State(state): State<AppState>,
    ApiSubject(s): ApiSubject,
    Path((ns, repo, number, change)): Path<(String, String, i32, String)>,
    OriginalUri(uri): OriginalUri,
    Query(f): Query<ListParams>,
    Query(p): Query<PageParams>,
) -> Result<Response, AppError> {
    let (offset, limit) = (p.offset(&state)?, p.limit(state.config.limits.api_page_max));
    let items = state
        .services
        .change_briefs(
            &s,
            (&ns, &repo, number),
            &change,
            f.commit.as_deref(),
            offset + limit + 1,
        )
        .await?;
    let items: Vec<BriefBody> = items
        .into_iter()
        .skip(offset)
        .map(BriefBody::from)
        .collect();
    Ok(page(&state, &uri, items, offset, limit))
}

pub fn router() -> Router<AppState> {
    Router::new().route_as(
        "/api/v1/repos/{ns}/{repo}/merge-requests/{number}/changes/{change}/briefs",
        Access::Resource,
        axum::routing::get(list_briefs).post(create_brief),
    )
}
