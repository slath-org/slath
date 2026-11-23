// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Linked merge requests on a merge request's page: the group in the
//! details column (link, unlink, move up and down) and the group's merge
//! box, which takes the place of the merge request's own.

use axum::extract::{Path, State};
use axum::response::{IntoResponse, Redirect, Response};
use axum::{Form, Router};
use serde::Deserialize;
use slath_service::mr_groups::{GroupMergeInput, GroupView, MergeMember};
use slath_service::{FieldError, ServiceError};

use super::PageCtx;
use super::merge_requests::mr_page;
use crate::AppState;
use crate::auth::RequireSession;
use crate::error::AppError;
use crate::routes::{Access, RouteExt};

/// One member as the details column and the merge box show it.
pub struct MemberRow {
    /// `ns/repo!n`.
    pub reference: String,
    pub href: String,
    pub title: String,
    pub tone: String,
    pub headline: String,
    /// The page's own merge request.
    pub current: bool,
    pub open: bool,
    /// It can move up (an open member after another open one) or down.
    pub up: bool,
    pub down: bool,
}

impl MemberRow {
    /// `ns/repo!n: headline`, for the merge box.
    pub fn line(&self) -> String {
        format!("{}: {}", self.reference, self.headline)
    }
}

/// The group on a member's page.
pub struct GroupPanel {
    pub members: Vec<MemberRow>,
    /// The group's state when something landed: tone and words.
    pub summary: Option<(&'static str, String)>,
    /// Why the last group merge stopped.
    pub failure: Option<String>,
    pub can_edit: bool,
    pub merging: bool,
    /// The open members, for the merge box.
    pub open: usize,
    /// The merge form's members (`ref head` per line) when the viewer may
    /// merge the group and every open member is ready.
    pub merge: Option<String>,
    /// Why the group can't be merged now, for the merge box.
    pub blocker: Option<String>,
}

impl GroupPanel {
    pub fn new(g: GroupView) -> Self {
        let open: Vec<usize> = g
            .members
            .iter()
            .enumerate()
            .filter(|(_, m)| m.state == "open")
            .map(|(i, _)| i)
            .collect();
        let (first, last) = (open.first().copied(), open.last().copied());
        let merged = g.members.iter().filter(|m| m.state == "merged").count();
        let summary = match g.state {
            "merged" => Some(("done", "Merged: every merge request landed.".to_owned())),
            "partially_landed" => Some((
                "waiting",
                format!("Partially landed: {merged} of {} merged.", g.members.len()),
            )),
            _ => None,
        };
        let failure = g.last_failure.as_ref().map(|f| match &f.member {
            Some(m) => format!("Merging stopped at {m}: {}", f.reason),
            None => format!("Merging stopped: {}", f.reason),
        });
        let blocker = if g.merging || open.is_empty() {
            None
        } else if !g.can_merge {
            Some(slath_service::mr_groups::NOT_ALL.to_owned())
        } else {
            g.open().find(|m| !m.mergeable).map(|m| {
                format!(
                    "{} isn't ready: {}.",
                    m.reference(),
                    m.headline.trim_end_matches('.')
                )
            })
        };
        let merge = (blocker.is_none() && !g.merging && g.can_merge && g.ready).then(|| {
            g.open()
                .map(|m| {
                    format!(
                        "{} {}",
                        m.reference(),
                        m.head_oid.clone().unwrap_or_default()
                    )
                })
                .collect::<Vec<_>>()
                .join("\n")
        });
        let members = g
            .members
            .iter()
            .enumerate()
            .map(|(i, m)| MemberRow {
                reference: m.reference(),
                href: format!("/{}/{}/merge-requests/{}", m.namespace, m.repo, m.number),
                title: m.title.clone(),
                tone: m.tone.clone(),
                headline: m.headline.clone(),
                current: m.current,
                open: m.state == "open",
                up: m.state == "open" && Some(i) != first,
                down: m.state == "open" && Some(i) != last,
            })
            .collect();
        Self {
            members,
            summary,
            failure,
            can_edit: g.can_edit,
            merging: g.merging,
            open: open.len(),
            merge,
            blocker,
        }
    }
}

fn number(s: &str) -> Result<i32, AppError> {
    s.parse::<i32>().map_err(|_| AppError::not_found())
}

fn back(ns: &str, repo: &str, n: i32, status: &str) -> Response {
    let to = if status.starts_with("group_merged") || status.starts_with("group_partial") {
        "group-merge"
    } else {
        "side-group"
    };
    Redirect::to(&format!(
        "/{ns}/{repo}/merge-requests/{n}?status={status}#{to}"
    ))
    .into_response()
}

/// A refusal as the page shows it, under `field`.
fn refusal(field: &str, e: ServiceError) -> Result<Vec<FieldError>, AppError> {
    let message = match e {
        ServiceError::Conflict(why) => why,
        ServiceError::Invalid(errs) => errs
            .into_iter()
            .map(|f| f.message)
            .collect::<Vec<_>>()
            .join(" "),
        // The page's own merge request is readable: a member isn't.
        ServiceError::NotFound => "There's no such merge request, or you can't read it.".to_owned(),
        ServiceError::Forbidden => {
            "You can change this group only if you may edit all its merge requests.".to_owned()
        }
        e => return Err(e.into()),
    };
    Ok(vec![FieldError {
        field: field.into(),
        code: "refused".into(),
        message,
    }])
}

/// The members' references in order, as the subject sees them; just this
/// merge request when it isn't linked.
async fn members(
    state: &AppState,
    subject: &slath_core::authz::Subject,
    (ns, repo, n): (&str, &str, i32),
) -> Result<Vec<String>, AppError> {
    Ok(
        match state.services.mr_group(subject, (ns, repo, n)).await? {
            Some(g) => g.members.iter().map(|m| m.reference()).collect(),
            None => vec![format!("{ns}/{repo}!{n}")],
        },
    )
}

async fn set(
    state: &AppState,
    ctx: PageCtx,
    subject: &slath_core::authz::Subject,
    (ns, repo, n): (&str, &str, i32),
    list: &[String],
    status: &str,
) -> Result<Response, AppError> {
    match state
        .services
        .set_mr_group(subject, (ns, repo, n), list)
        .await
    {
        Ok(_) => Ok(back(ns, repo, n, status)),
        Err(e) => {
            let errors = refusal("group", e)?;
            mr_page(state, ctx, subject, (ns, repo, n), None, errors).await
        }
    }
}

#[derive(Deserialize)]
struct LinkForm {
    #[serde(default)]
    reference: String,
}

/// "Link a merge request": adds one at the end of the group (or makes one).
async fn link(
    State(state): State<AppState>,
    ctx: PageCtx,
    RequireSession(s): RequireSession,
    Path((ns, repo, n)): Path<(String, String, String)>,
    Form(f): Form<LinkForm>,
) -> Result<Response, AppError> {
    let n = number(&n)?;
    let mut list = members(&state, &s.subject, (&ns, &repo, n)).await?;
    list.push(f.reference.trim().to_owned());
    set(&state, ctx, &s.subject, (&ns, &repo, n), &list, "linked").await
}

#[derive(Deserialize)]
struct MoveForm {
    #[serde(default)]
    member: String,
    #[serde(default)]
    dir: String,
}

/// Move up / Move down: swaps a member with its neighbor.
async fn move_member(
    State(state): State<AppState>,
    ctx: PageCtx,
    RequireSession(s): RequireSession,
    Path((ns, repo, n)): Path<(String, String, String)>,
    Form(f): Form<MoveForm>,
) -> Result<Response, AppError> {
    let n = number(&n)?;
    let mut list = members(&state, &s.subject, (&ns, &repo, n)).await?;
    if let Some(i) = list.iter().position(|r| *r == f.member) {
        match f.dir.as_str() {
            "up" if i > 0 => list.swap(i - 1, i),
            "down" if i + 1 < list.len() => list.swap(i, i + 1),
            _ => {}
        }
    }
    set(
        &state,
        ctx,
        &s.subject,
        (&ns, &repo, n),
        &list,
        "group_moved",
    )
    .await
}

/// Unlinks the page's merge request from its group.
async fn unlink(
    State(state): State<AppState>,
    ctx: PageCtx,
    RequireSession(s): RequireSession,
    Path((ns, repo, n)): Path<(String, String, String)>,
) -> Result<Response, AppError> {
    let n = number(&n)?;
    match state
        .services
        .unlink_mr_group(&s.subject, (&ns, &repo, n))
        .await
    {
        Ok(()) => Ok(back(&ns, &repo, n, "unlinked")),
        Err(e) => {
            let errors = refusal("group", e)?;
            mr_page(&state, ctx, &s.subject, (&ns, &repo, n), None, errors).await
        }
    }
}

#[derive(Deserialize)]
struct GroupMergeForm {
    /// `ref head` per line, every open member in order.
    #[serde(default)]
    members: String,
}

/// "Merge N merge requests in order".
async fn merge(
    State(state): State<AppState>,
    ctx: PageCtx,
    RequireSession(s): RequireSession,
    Path((ns, repo, n)): Path<(String, String, String)>,
    Form(f): Form<GroupMergeForm>,
) -> Result<Response, AppError> {
    let n = number(&n)?;
    let input = GroupMergeInput {
        members: f
            .members
            .lines()
            .filter_map(|l| l.trim().split_once(' '))
            .map(|(r, h)| MergeMember {
                merge_request: r.to_owned(),
                expected_head_oid: h.trim().to_owned(),
                strategy: None,
            })
            .collect(),
        delete_source: None,
    };
    match state
        .services
        .merge_mr_group(&s.subject, (&ns, &repo, n), &input)
        .await
    {
        Ok(o) if o.failure.is_some() => Ok(back(&ns, &repo, n, "group_partial")),
        Ok(_) => Ok(back(&ns, &repo, n, "group_merged")),
        Err(e) => {
            let errors = refusal("group_merge", e)?;
            mr_page(&state, ctx, &s.subject, (&ns, &repo, n), None, errors).await
        }
    }
}

pub fn router() -> Router<AppState> {
    use axum::routing::post;
    Router::new()
        .route_as(
            "/{ns}/{repo}/merge-requests/{n}/group/link",
            Access::Resource,
            post(link),
        )
        .route_as(
            "/{ns}/{repo}/merge-requests/{n}/group/move",
            Access::Resource,
            post(move_member),
        )
        .route_as(
            "/{ns}/{repo}/merge-requests/{n}/group/unlink",
            Access::Resource,
            post(unlink),
        )
        .route_as(
            "/{ns}/{repo}/merge-requests/{n}/group/merge",
            Access::Resource,
            post(merge),
        )
}
