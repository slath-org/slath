// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! The state of a merge request or a work order in one read: what an
//! agent needs to bring a person up to speed. Each item is what one of the
//! individual endpoints returns, run through the same router as the same
//! caller, so authorization and every field are theirs by construction.
//! Nothing is stored and no model is asked.

use axum::Router;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::http::request::Parts;
use axum::response::{IntoResponse, Response};
use serde::Serialize;
use serde_json::Value;
use utoipa::ToSchema;

use super::ApiSubject;
use crate::AppState;
use crate::error::AppError;
use crate::routes::{Access, RouteExt};

/// One item of a state read.
#[derive(Serialize, ToSchema)]
pub struct StateItem {
    /// `goal`, `merge_request`, `change_version`, `status`, `approval`,
    /// `verdict`, `open_thread`, or `merge_blocker`.
    pub kind: &'static str,
    /// The API path of the record it comes from.
    pub record: String,
    /// `observed` (Slath saw it), `reported` (an authenticated reporter
    /// says so), `claimed` (someone states it), or `decided` (a person's
    /// or bot's verdict).
    pub evidence: &'static str,
    /// What that record's endpoint returns for it.
    #[schema(value_type = Object)]
    pub value: Value,
}

/// A state read.
#[derive(Serialize, ToSchema)]
pub struct StateBody {
    pub items: Vec<StateItem>,
}

/// Runs `GET path` as the caller; its JSON body, or the refusal as is.
/// A refusal passed on as the record's endpoint gave it.
type Refusal = Box<Response>;

async fn get(state: &AppState, parts: &Parts, path: &str) -> Result<Value, Refusal> {
    match super::mcp::run(state, parts, path).await {
        Ok((status, _, body)) if status.is_success() => {
            serde_json::from_slice(&body).map_err(|e| {
                tracing::error!(error = %e, path, "a state read's record isn't JSON");
                Box::new(AppError::internal(&e).into_response())
            })
        }
        Ok((status, ct, body)) => {
            tracing::debug!(%status, path, "a state read's record refused");
            Err(Box::new(
                (status, [(axum::http::header::CONTENT_TYPE, ct)], body).into_response(),
            ))
        }
        Err(e) => {
            tracing::error!(error = %e, path, "reading a state read's record failed");
            Err(Box::new(AppError::internal(&e).into_response()))
        }
    }
}

fn items_of(v: &Value) -> Vec<Value> {
    match v {
        Value::Array(a) => a.clone(),
        Value::Object(o) => o
            .get("items")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default(),
        _ => Vec::new(),
    }
}

/// The items of merge request `number` (`base` is its API path).
async fn mr_items(
    state: &AppState,
    parts: &Parts,
    (ns, repo): (&str, &str),
    number: i32,
) -> Result<Vec<StateItem>, Refusal> {
    let base = format!("/api/v1/repos/{ns}/{repo}/merge-requests/{number}");
    let mut out = Vec::new();
    let mr = get(state, parts, &base).await?;
    out.push(StateItem {
        kind: "merge_request",
        record: base.clone(),
        evidence: "observed",
        value: mr.clone(),
    });
    let stack = format!("{base}/stack");
    let s = get(state, parts, &stack).await?;
    for e in s
        .get("entries")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
    {
        out.push(StateItem {
            kind: "change_version",
            record: stack.clone(),
            evidence: "observed",
            value: e,
        });
    }
    if let Some(head) = mr.get("head_oid").and_then(Value::as_str) {
        let path = format!("/api/v1/repos/{ns}/{repo}/commits/{head}/statuses");
        for st in items_of(&get(state, parts, &path).await?) {
            out.push(StateItem {
                kind: "status",
                record: path.clone(),
                evidence: "reported",
                value: st,
            });
        }
    }
    let reviews = format!("{base}/reviews");
    for r in items_of(&get(state, parts, &reviews).await?) {
        let approval = r.get("verdict").and_then(Value::as_str) == Some("approve");
        out.push(StateItem {
            kind: if approval { "approval" } else { "verdict" },
            record: reviews.clone(),
            evidence: "decided",
            value: r,
        });
    }
    let threads = format!("{base}/threads?resolved=false");
    for t in items_of(&get(state, parts, &threads).await?) {
        out.push(StateItem {
            kind: "open_thread",
            record: format!("{base}/threads"),
            evidence: "claimed",
            value: t,
        });
    }
    let mergeability = format!("{base}/mergeability");
    let mb = get(state, parts, &mergeability).await?;
    for r in mb
        .get("requirements")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter(|r| r.get("state").and_then(Value::as_str) != Some("met"))
    {
        out.push(StateItem {
            kind: "merge_blocker",
            record: mergeability.clone(),
            evidence: "observed",
            value: r,
        });
    }
    Ok(out)
}

/// A merge request's state in one read: the merge request, its change
/// versions, the statuses on its exact head, approvals and other verdicts
/// (with `stale`), open threads, and what blocks merging. Each item is
/// what its `record` returns for it, with the same authorization.
#[utoipa::path(get, path = "/api/v1/repos/{ns}/{repo}/merge-requests/{number}/state", tag = "merge requests", security((), ("token" = ["repo:read"])),
    params(("ns" = String, Path), ("repo" = String, Path), ("number" = i32, Path)),
    responses((status = 200, body = StateBody), (status = 404, body = super::Problem)))]
pub async fn get_mr_state(
    State(state): State<AppState>,
    ApiSubject(_s): ApiSubject,
    Path((ns, repo, number)): Path<(String, String, i32)>,
    parts: Parts,
) -> Response {
    match mr_items(&state, &parts, (&ns, &repo), number).await {
        Ok(items) => (StatusCode::OK, axum::Json(StateBody { items })).into_response(),
        Err(r) => *r,
    }
}

/// A work order's state in one read: its goal (the order itself), then
/// the items of each of its merge requests as `…/merge-requests/{n}/state`
/// returns them.
#[utoipa::path(get, path = "/api/v1/repos/{ns}/{repo}/work-orders/{number}/state", tag = "work orders", security((), ("token" = ["repo:read"])),
    params(("ns" = String, Path), ("repo" = String, Path), ("number" = i32, Path)),
    responses((status = 200, body = StateBody), (status = 403, body = super::Problem), (status = 404, body = super::Problem)))]
pub async fn get_order_state(
    State(state): State<AppState>,
    ApiSubject(s): ApiSubject,
    Path((ns, repo, number)): Path<(String, String, i32)>,
    parts: Parts,
) -> Response {
    let path = format!("/api/v1/repos/{ns}/{repo}/work-orders/{number}");
    let order = match get(&state, &parts, &path).await {
        Ok(o) => o,
        Err(r) => return *r,
    };
    let mrs = match state
        .services
        .work_order_mrs(&s, (&ns, &repo), number)
        .await
    {
        Ok(m) => m,
        Err(e) => return AppError::from(e).into_response(),
    };
    let mut items = vec![StateItem {
        kind: "goal",
        record: path,
        evidence: "claimed",
        value: order,
    }];
    for n in mrs {
        match mr_items(&state, &parts, (&ns, &repo), n).await {
            Ok(i) => items.extend(i),
            Err(r) => return *r,
        }
    }
    (StatusCode::OK, axum::Json(StateBody { items })).into_response()
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route_as(
            "/api/v1/repos/{ns}/{repo}/merge-requests/{number}/state",
            Access::Resource,
            axum::routing::get(get_mr_state),
        )
        .route_as(
            "/api/v1/repos/{ns}/{repo}/work-orders/{number}/state",
            Access::Resource,
            axum::routing::get(get_order_state),
        )
}
