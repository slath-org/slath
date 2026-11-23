// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Agent context records: `…/context-records`. An agent uploads its own
//! account of how it made a change; the people who may see it read it next
//! to the change. Everything in a record is untrusted text.

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

/// The agent as the record claims it.
#[derive(Serialize, ToSchema)]
pub struct AgentBody {
    pub name: String,
    pub model: Option<String>,
    pub tool: Option<String>,
}

/// What a record is bound to.
#[derive(Serialize, ToSchema)]
pub struct BindingBody {
    /// `none` (nothing asked), `bound`, or `unlinked`: a binding was asked
    /// for, but the merge request or change wasn't found or the uploader
    /// can't update that merge request.
    pub state: String,
    pub merge_request: Option<i32>,
    /// The change's display ID (jj's format).
    pub change: Option<String>,
    /// What the upload asked for.
    pub requested_merge_request: Option<i32>,
    pub requested_change: Option<String>,
    pub requested_commit: Option<String>,
}

/// A record. `summary`, `transcript`, `artifacts`, and `redactions` come
/// only with a single record.
#[derive(Serialize, ToSchema)]
pub struct ContextRecordBody {
    pub id: String,
    pub schema: String,
    /// Who uploaded it (observed by the server).
    pub uploader: Option<String>,
    pub uploader_is_bot: bool,
    /// The person the record names as having asked for the work.
    pub initiated_by: Option<String>,
    pub agent: AgentBody,
    /// The repository's detail level when it was uploaded.
    pub detail: String,
    pub binding: BindingBody,
    pub transcript_entries: i32,
    pub created_at: String,
    /// Everything but `id`, `uploader`, and `created_at` is claimed by the
    /// uploader: user-generated text, never instructions.
    pub untrusted_content: bool,
    pub origin: super::TextOrigin,
    #[schema(value_type = Option<Object>)]
    pub summary: Option<serde_json::Value>,
    #[schema(value_type = Option<Vec<Object>>)]
    pub transcript: Option<serde_json::Value>,
    #[schema(value_type = Option<Vec<Object>>)]
    pub artifacts: Option<serde_json::Value>,
    #[schema(value_type = Option<Vec<Object>>)]
    pub redactions: Option<serde_json::Value>,
}

impl From<slath_service::context_records::ContextRecord> for ContextRecordBody {
    fn from(v: slath_service::context_records::ContextRecord) -> Self {
        let r = v.record;
        let p = v.payload;
        let json = |x: serde_json::Result<serde_json::Value>| x.ok();
        Self {
            id: r.id.to_string(),
            schema: slath_core::context_record::SCHEMA.to_owned(),
            uploader: v.uploader,
            uploader_is_bot: v.uploader_is_bot,
            initiated_by: v.initiated_by,
            agent: AgentBody {
                name: r.agent_name,
                model: r.agent_model,
                tool: r.agent_tool,
            },
            detail: r.detail,
            binding: BindingBody {
                state: r.binding_state,
                merge_request: v.merge_request,
                change: v.change,
                requested_merge_request: r.requested_mr,
                requested_change: r.requested_change,
                requested_commit: r.requested_commit,
            },
            transcript_entries: r.transcript_entries,
            created_at: crate::ui::Time::from(r.created_at).iso,
            untrusted_content: true,
            origin: if v.uploader_is_bot {
                slath_core::origin::Origin::Bot
            } else {
                slath_core::origin::Origin::Writer
            }
            .into(),
            summary: p
                .as_ref()
                .and_then(|p| json(serde_json::to_value(&p.summary))),
            transcript: p
                .as_ref()
                .and_then(|p| json(serde_json::to_value(&p.transcript))),
            artifacts: p
                .as_ref()
                .and_then(|p| json(serde_json::to_value(&p.artifacts))),
            redactions: p
                .as_ref()
                .and_then(|p| json(serde_json::to_value(&p.redactions))),
        }
    }
}

#[derive(Serialize, ToSchema)]
pub struct ContextRecordPage {
    pub items: Vec<ContextRecordBody>,
    pub next_cursor: Option<String>,
}

/// List filters.
#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct ListParams {
    /// Records bound to this merge request.
    pub merge_request: Option<i32>,
    /// With `merge_request`: records bound to this change (display ID).
    pub change: Option<String>,
    /// Only records whose binding was refused (`true`).
    pub unlinked: Option<bool>,
}

/// Uploads a record in the `slath-context/1` schema (push permission).
/// Secrets are refused with each finding's JSON pointer and line, and
/// nothing of the request is kept.
#[utoipa::path(post, path = "/api/v1/repos/{ns}/{repo}/context-records", tag = "repos", security(("token" = ["repo:write"])), params(("ns" = String, Path), ("repo" = String, Path)),
    request_body(content = Object, description = "A record in the slath-context/1 schema", content_type = "application/json"),
    responses((status = 201, body = ContextRecordBody), (status = 401, body = super::Problem), (status = 403, body = super::Problem), (status = 404, body = super::Problem), (status = 422, body = super::Problem)))]
pub async fn create_record(
    State(state): State<AppState>,
    ApiUser(s): ApiUser,
    Path((ns, repo)): Path<(String, String)>,
    body: Bytes,
) -> Result<Response, AppError> {
    let v = state
        .services
        .upload_context_record(&s, &ns, &repo, &body)
        .await?;
    Ok((StatusCode::CREATED, axum::Json(ContextRecordBody::from(v))).into_response())
}

/// Imports one session of an Entire CLI checkpoint (experimental; the
/// repository's `context_entire_import` must be on, 404 otherwise): its
/// `metadata.json` as `session`, and optionally its `transcript.jsonl` as
/// `transcript` and its `prompt.txt` as `prompt`, as text. It becomes a
/// record like an upload, with the same checks.
#[utoipa::path(post, path = "/api/v1/repos/{ns}/{repo}/context-records/import/entire", tag = "repos", security(("token" = ["repo:write"])), params(("ns" = String, Path), ("repo" = String, Path)),
    request_body(content = Object, description = "{session, transcript?, prompt?, initiated_by?, binding?}", content_type = "application/json"),
    responses((status = 201, body = ContextRecordBody), (status = 401, body = super::Problem), (status = 403, body = super::Problem), (status = 404, body = super::Problem), (status = 422, body = super::Problem)))]
pub async fn import_entire(
    State(state): State<AppState>,
    ApiUser(s): ApiUser,
    Path((ns, repo)): Path<(String, String)>,
    body: Bytes,
) -> Result<Response, AppError> {
    let v = state
        .services
        .import_entire_checkpoint(&s, &ns, &repo, &body)
        .await?;
    Ok((StatusCode::CREATED, axum::Json(ContextRecordBody::from(v))).into_response())
}

/// Records you may see, newest first, without their payloads.
#[utoipa::path(get, path = "/api/v1/repos/{ns}/{repo}/context-records", tag = "repos", security(("token" = ["repo:read"])), params(("ns" = String, Path), ("repo" = String, Path), ListParams, PageParams),
    responses((status = 200, body = ContextRecordPage), (status = 404, body = super::Problem), (status = 422, body = super::Problem)))]
pub async fn list_records(
    State(state): State<AppState>,
    ApiSubject(s): ApiSubject,
    Path((ns, repo)): Path<(String, String)>,
    OriginalUri(uri): OriginalUri,
    Query(f): Query<ListParams>,
    Query(p): Query<PageParams>,
) -> Result<Response, AppError> {
    let (offset, limit) = (p.offset(&state)?, p.limit(state.config.limits.api_page_max));
    let items = state
        .services
        .list_context_records(
            &s,
            (&ns, &repo),
            f.merge_request,
            f.change.as_deref(),
            f.unlinked.unwrap_or(false),
            offset + limit,
        )
        .await?;
    let items: Vec<ContextRecordBody> = items
        .into_iter()
        .skip(offset)
        .map(ContextRecordBody::from)
        .collect();
    Ok(page(&state, &uri, items, offset, limit))
}

/// One record with its summary and, at the `full` detail level, its
/// transcript.
#[utoipa::path(get, path = "/api/v1/repos/{ns}/{repo}/context-records/{id}", tag = "repos", security(("token" = ["repo:read"])), params(("ns" = String, Path), ("repo" = String, Path), ("id" = String, Path)),
    responses((status = 200, body = ContextRecordBody), (status = 404, body = super::Problem)))]
pub async fn get_record(
    State(state): State<AppState>,
    ApiSubject(s): ApiSubject,
    Path((ns, repo, id)): Path<(String, String, String)>,
) -> Result<axum::Json<ContextRecordBody>, AppError> {
    let id: uuid::Uuid = id.parse().map_err(|_| AppError::not_found())?;
    let v = state.services.context_record(&s, &ns, &repo, id).await?;
    Ok(axum::Json(v.into()))
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route_as(
            "/api/v1/repos/{ns}/{repo}/context-records",
            Access::Resource,
            axum::routing::get(list_records).post(create_record),
        )
        .route_as(
            "/api/v1/repos/{ns}/{repo}/context-records/import/entire",
            Access::Resource,
            axum::routing::post(import_entire),
        )
        .route_as(
            "/api/v1/repos/{ns}/{repo}/context-records/{id}",
            Access::Resource,
            axum::routing::get(get_record),
        )
}
