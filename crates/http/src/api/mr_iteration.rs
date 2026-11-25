// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! `GET …/merge-requests/{n}/iteration`: one call per round of an author
//! or agent, made of what the individual endpoints return.

use axum::extract::{Path, Query, State};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};

use super::ApiSubject;
use super::merge_requests::MergeabilityBody;
use super::mr_revisions::RevisionBody;
use super::mr_threads::ThreadBody;
use super::statuses::CommitStatus;
use crate::AppState;
use crate::error::AppError;
use crate::routes::{Access, RouteExt};

/// The revision interdiff since an earlier revision.
#[derive(Serialize, ToSchema)]
pub struct IterationInterdiff {
    pub from: i32,
    pub to: i32,
    /// What `GET …/interdiff?from=&to=` serves (`git range-diff`); `null`
    /// past `limits.iteration_interdiff_max_bytes` (1 MiB).
    pub text: Option<String>,
    pub too_large: bool,
}

/// What the next round needs.
#[derive(Serialize, ToSchema)]
pub struct IterationBody {
    /// The current revision, as the last item of `…/revisions`; `null`
    /// before the first one. Everything else refers to its head.
    pub revision: Option<RevisionBody>,
    /// With `since`: what changed from that revision to the current one.
    /// `null` without `since`, when it's the current revision, or when a
    /// revision has no merge base.
    pub interdiff: Option<IterationInterdiff>,
    /// The latest status of each required context on the head, as
    /// `…/commits/{oid}/statuses` returns it, when it's `failure` or
    /// `error`. Its description, target URL, and annotations say what
    /// failed.
    pub failing_checks: Vec<CommitStatus>,
    /// The unresolved threads, as `…/threads?resolved=false` returns them.
    pub threads: Vec<ThreadBody>,
    /// As `…/mergeability` returns it.
    pub mergeability: MergeabilityBody,
    /// The work order the merge request belongs to (its source is a branch
    /// under `wo/<n>/`), as `…/work-orders/{n}` returns it; else `null`.
    pub work_order: Option<super::work_orders::OrderBody>,
}

#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct IterationQuery {
    /// The revision you last worked on; adds the interdiff since then.
    pub since: Option<i32>,
}

/// The current revision, the interdiff since `since`, failing required
/// checks, unresolved threads, and mergeability, in one call. 422 when
/// `since` isn't a revision of the merge request.
#[utoipa::path(get, path = "/api/v1/repos/{ns}/{repo}/merge-requests/{number}/iteration", tag = "merge requests", security((), ("token" = ["repo:read"])), params(("ns" = String, Path), ("repo" = String, Path), ("number" = i32, Path), IterationQuery),
    responses((status = 200, body = IterationBody), (status = 404, body = super::Problem), (status = 422, body = super::Problem)))]
pub async fn get_iteration(
    State(state): State<AppState>,
    ApiSubject(s): ApiSubject,
    Path((ns, repo, number)): Path<(String, String, i32)>,
    Query(q): Query<IterationQuery>,
) -> Result<Json<IterationBody>, AppError> {
    let it = state
        .services
        .mr_iteration(&s, (&ns, &repo, number), q.since)
        .await?;
    Ok(Json(IterationBody {
        revision: it.revision.map(Into::into),
        interdiff: it.interdiff.map(|d| IterationInterdiff {
            from: d.from,
            to: d.to,
            text: d.text,
            too_large: d.too_large,
        }),
        failing_checks: it.failing_checks.into_iter().map(Into::into).collect(),
        threads: it.threads.into_iter().map(Into::into).collect(),
        mergeability: it.mergeability.into(),
        work_order: it.work_order.map(Into::into),
    }))
}

pub fn router() -> Router<AppState> {
    Router::new().route_as(
        "/api/v1/repos/{ns}/{repo}/merge-requests/{number}/iteration",
        Access::Resource,
        axum::routing::get(get_iteration),
    )
}
