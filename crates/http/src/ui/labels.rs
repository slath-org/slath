// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Label and milestone pages, and applying labels, assignees, and a
//! milestone to an issue.

use askama::Template;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Redirect, Response};
use axum::{Form, Router};
use serde::Deserialize;
use slath_core::authz::{Action, Subject};
use slath_db::labels::{Label, Milestone};
use slath_service::{FieldError, ServiceError};

use super::repos::RepoHeader;
use super::settings::error_for;
use super::{PageCtx, render};
use crate::AppState;
use crate::auth::{MaybeSession, RequireSession};
use crate::error::AppError;
use crate::routes::{Access, RouteExt};

#[derive(Deserialize, Default)]
struct StatusQuery {
    status: Option<String>,
    /// Prefills the form with this label or milestone.
    edit: Option<String>,
}

fn message(q: &StatusQuery) -> Option<&'static str> {
    match q.status.as_deref() {
        Some("saved") => Some("Saved."),
        Some("deleted") => Some("Deleted."),
        _ => None,
    }
}

fn id(s: &str) -> Result<uuid::Uuid, AppError> {
    s.parse().map_err(|_| AppError::not_found())
}

/// A filter link for the issue list (`label:"name"`-style query).
pub fn filter_href(ns: &str, repo: &str, key: &str, value: &str) -> String {
    let mut v = String::new();
    for b in value.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
            v.push(char::from(b));
        } else {
            v.push_str(&format!("%{b:02X}"));
        }
    }
    format!("/{ns}/{repo}/issues?{key}={v}")
}

#[derive(Template)]
#[template(path = "repo/labels.html")]
struct LabelsPage {
    ctx: PageCtx,
    header: RepoHeader,
    labels: Vec<Label>,
    manage: bool,
    message: Option<&'static str>,
    errors: Vec<FieldError>,
    /// The label the form edits (`None`: a new label).
    editing: Option<uuid::Uuid>,
    name: String,
    color: String,
    description: String,
}

impl LabelsPage {
    fn error(&self, field: &str) -> Option<&str> {
        error_for(&self.errors, field)
    }
    fn href(&self, l: &Label) -> String {
        filter_href(&self.header.namespace, &self.header.name, "labels", &l.name)
    }
}

#[allow(clippy::too_many_arguments)]
async fn labels_page(
    state: &AppState,
    ctx: PageCtx,
    subject: &Subject,
    (ns, repo): (&str, &str),
    message: Option<&'static str>,
    errors: Vec<FieldError>,
    editing: Option<uuid::Uuid>,
    form: LabelForm,
) -> Result<Response, AppError> {
    let (a, labels) = state.services.labels(subject, ns, repo).await?;
    let status = if errors.is_empty() {
        StatusCode::OK
    } else {
        StatusCode::UNPROCESSABLE_ENTITY
    };
    let mut res = render(&LabelsPage {
        header: RepoHeader::load(state, &a, subject, "issues").await,
        manage: a.can(subject, Action::LabelsManage),
        labels,
        message,
        errors,
        editing,
        name: form.name,
        color: if form.color.is_empty() {
            "#1f6feb".to_owned()
        } else {
            form.color
        },
        description: form.description,
        ctx,
    });
    *res.status_mut() = status;
    Ok(res)
}

async fn labels(
    State(state): State<AppState>,
    ctx: PageCtx,
    session: MaybeSession,
    Path((ns, repo)): Path<(String, String)>,
    Query(q): Query<StatusQuery>,
) -> Result<Response, AppError> {
    let subject = session.subject();
    let mut form = LabelForm::default();
    let mut editing = None;
    if let Some(e) = q.edit.as_deref().and_then(|e| e.parse::<uuid::Uuid>().ok()) {
        let (_, all) = state.services.labels(&subject, &ns, &repo).await?;
        let l = all
            .into_iter()
            .find(|l| l.id == e)
            .ok_or_else(AppError::not_found)?;
        editing = Some(l.id);
        form = LabelForm {
            name: l.name,
            color: format!("#{}", l.color),
            description: l.description.unwrap_or_default(),
        };
    }
    labels_page(
        &state,
        ctx,
        &subject,
        (&ns, &repo),
        message(&q),
        Vec::new(),
        editing,
        form,
    )
    .await
}

#[derive(Deserialize, Default)]
struct LabelForm {
    #[serde(default)]
    name: String,
    #[serde(default)]
    color: String,
    #[serde(default)]
    description: String,
}

async fn save_label(
    state: AppState,
    ctx: PageCtx,
    subject: &Subject,
    (ns, repo): (&str, &str),
    label: Option<uuid::Uuid>,
    f: LabelForm,
) -> Result<Response, AppError> {
    match state
        .services
        .save_label(subject, ns, repo, label, &f.name, &f.color, &f.description)
        .await
    {
        Ok(()) => Ok(Redirect::to(&format!("/{ns}/{repo}/labels?status=saved")).into_response()),
        Err(ServiceError::Invalid(errors)) => {
            labels_page(&state, ctx, subject, (ns, repo), None, errors, label, f).await
        }
        Err(e) => Err(e.into()),
    }
}

async fn create_label(
    State(state): State<AppState>,
    ctx: PageCtx,
    RequireSession(s): RequireSession,
    Path((ns, repo)): Path<(String, String)>,
    Form(f): Form<LabelForm>,
) -> Result<Response, AppError> {
    save_label(state, ctx, &s.subject, (&ns, &repo), None, f).await
}

async fn update_label(
    State(state): State<AppState>,
    ctx: PageCtx,
    RequireSession(s): RequireSession,
    Path((ns, repo, l)): Path<(String, String, String)>,
    Form(f): Form<LabelForm>,
) -> Result<Response, AppError> {
    save_label(state, ctx, &s.subject, (&ns, &repo), Some(id(&l)?), f).await
}

async fn delete_label(
    State(state): State<AppState>,
    RequireSession(s): RequireSession,
    Path((ns, repo, l)): Path<(String, String, String)>,
) -> Result<Response, AppError> {
    state
        .services
        .delete_label(&s.subject, &ns, &repo, id(&l)?)
        .await?;
    Ok(Redirect::to(&format!("/{ns}/{repo}/labels?status=deleted")).into_response())
}

struct MilestoneView {
    m: Milestone,
    due: String,
    total: i64,
    percent: i64,
    href: String,
}

#[derive(Template)]
#[template(path = "repo/milestones.html")]
struct MilestonesPage {
    ctx: PageCtx,
    header: RepoHeader,
    milestones: Vec<MilestoneView>,
    manage: bool,
    message: Option<&'static str>,
    errors: Vec<FieldError>,
    editing: Option<uuid::Uuid>,
    title: String,
    description: String,
    due_on: String,
    open: bool,
}

impl MilestonesPage {
    fn error(&self, field: &str) -> Option<&str> {
        error_for(&self.errors, field)
    }
}

fn date(d: time::Date) -> String {
    let fmt = time::macros::format_description!("[year]-[month]-[day]");
    d.format(&fmt).unwrap_or_default()
}

#[allow(clippy::too_many_arguments)]
async fn milestones_page(
    state: &AppState,
    ctx: PageCtx,
    subject: &Subject,
    (ns, repo): (&str, &str),
    message: Option<&'static str>,
    errors: Vec<FieldError>,
    editing: Option<uuid::Uuid>,
    form: MilestoneForm,
) -> Result<Response, AppError> {
    let (a, ms) = state.services.milestones(subject, ns, repo).await?;
    let status = if errors.is_empty() {
        StatusCode::OK
    } else {
        StatusCode::UNPROCESSABLE_ENTITY
    };
    let mut res = render(&MilestonesPage {
        manage: a.can(subject, Action::MilestonesManage),
        milestones: ms
            .into_iter()
            .map(|m| {
                let total = m.open_issues + m.closed_issues;
                MilestoneView {
                    due: m.due_on.map(date).unwrap_or_default(),
                    percent: if total == 0 {
                        0
                    } else {
                        m.closed_issues * 100 / total
                    },
                    total,
                    href: format!(
                        "{}&state=all",
                        filter_href(&a.repo.namespace, &a.repo.name, "milestone", &m.title)
                    ),
                    m,
                }
            })
            .collect(),
        header: RepoHeader::load(state, &a, subject, "issues").await,
        message,
        errors,
        editing,
        title: form.title,
        description: form.description,
        due_on: form.due_on,
        open: form.state != "closed",
        ctx,
    });
    *res.status_mut() = status;
    Ok(res)
}

async fn milestones(
    State(state): State<AppState>,
    ctx: PageCtx,
    session: MaybeSession,
    Path((ns, repo)): Path<(String, String)>,
    Query(q): Query<StatusQuery>,
) -> Result<Response, AppError> {
    let subject = session.subject();
    let mut form = MilestoneForm::default();
    let mut editing = None;
    if let Some(e) = q.edit.as_deref().and_then(|e| e.parse::<uuid::Uuid>().ok()) {
        let (_, all) = state.services.milestones(&subject, &ns, &repo).await?;
        let m = all
            .into_iter()
            .find(|m| m.id == e)
            .ok_or_else(AppError::not_found)?;
        editing = Some(m.id);
        form = MilestoneForm {
            title: m.title,
            description: m.description.unwrap_or_default(),
            due_on: m.due_on.map(date).unwrap_or_default(),
            state: m.state,
        };
    }
    milestones_page(
        &state,
        ctx,
        &subject,
        (&ns, &repo),
        message(&q),
        Vec::new(),
        editing,
        form,
    )
    .await
}

#[derive(Deserialize, Default)]
struct MilestoneForm {
    #[serde(default)]
    title: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    due_on: String,
    /// `closed` closes the milestone; anything else keeps it open.
    #[serde(default)]
    state: String,
}

async fn save_milestone(
    state: AppState,
    ctx: PageCtx,
    subject: &Subject,
    (ns, repo): (&str, &str),
    milestone: Option<uuid::Uuid>,
    f: MilestoneForm,
) -> Result<Response, AppError> {
    match state
        .services
        .save_milestone(
            subject,
            ns,
            repo,
            milestone,
            &f.title,
            &f.description,
            &f.due_on,
            f.state != "closed",
        )
        .await
    {
        Ok(()) => {
            Ok(Redirect::to(&format!("/{ns}/{repo}/milestones?status=saved")).into_response())
        }
        Err(ServiceError::Invalid(errors)) => {
            milestones_page(&state, ctx, subject, (ns, repo), None, errors, milestone, f).await
        }
        Err(e) => Err(e.into()),
    }
}

async fn create_milestone(
    State(state): State<AppState>,
    ctx: PageCtx,
    RequireSession(s): RequireSession,
    Path((ns, repo)): Path<(String, String)>,
    Form(f): Form<MilestoneForm>,
) -> Result<Response, AppError> {
    save_milestone(state, ctx, &s.subject, (&ns, &repo), None, f).await
}

async fn update_milestone(
    State(state): State<AppState>,
    ctx: PageCtx,
    RequireSession(s): RequireSession,
    Path((ns, repo, m)): Path<(String, String, String)>,
    Form(f): Form<MilestoneForm>,
) -> Result<Response, AppError> {
    save_milestone(state, ctx, &s.subject, (&ns, &repo), Some(id(&m)?), f).await
}

async fn delete_milestone(
    State(state): State<AppState>,
    RequireSession(s): RequireSession,
    Path((ns, repo, m)): Path<(String, String, String)>,
) -> Result<Response, AppError> {
    state
        .services
        .delete_milestone(&s.subject, &ns, &repo, id(&m)?)
        .await?;
    Ok(Redirect::to(&format!("/{ns}/{repo}/milestones?status=deleted")).into_response())
}

fn issue_number(s: &str) -> Result<i32, AppError> {
    s.parse::<i32>()
        .ok()
        .filter(|n| *n > 0)
        .ok_or_else(AppError::not_found)
}

/// Applies labels, assignees, or a milestone from the issue sidebar.
async fn triage(
    State(state): State<AppState>,
    ctx: PageCtx,
    RequireSession(s): RequireSession,
    Path((ns, repo, n, what)): Path<(String, String, String, String)>,
    body: axum::body::Bytes,
) -> Result<Response, AppError> {
    let n = issue_number(&n)?;
    let pairs = crate::auth::form_pairs(&body);
    let all = |k: &str| -> Vec<String> {
        pairs
            .iter()
            .filter(|(name, _)| name == k)
            .map(|(_, v)| v.clone())
            .collect()
    };
    let r = match what.as_str() {
        "labels" => {
            state
                .services
                .set_issue_labels(&s.subject, &ns, &repo, n, &all("labels"))
                .await
        }
        "assignees" => {
            let names: Vec<String> = all("assignees")
                .iter()
                .flat_map(|v| v.split([',', ' ']))
                .map(str::to_owned)
                .collect();
            state
                .services
                .set_issue_assignees(&s.subject, &ns, &repo, n, &names)
                .await
        }
        "milestone" => {
            let m = all("milestone").into_iter().next();
            state
                .services
                .set_issue_milestone(&s.subject, &ns, &repo, n, m.as_deref())
                .await
        }
        _ => return Err(AppError::not_found()),
    };
    match r {
        Ok(()) => Ok(Redirect::to(&format!("/{ns}/{repo}/issues/{n}")).into_response()),
        Err(ServiceError::Invalid(errors)) => {
            super::issues::issue_page(&state, ctx, &s.subject, (&ns, &repo, n), None, errors).await
        }
        Err(e) => Err(e.into()),
    }
}

pub fn router() -> Router<AppState> {
    use axum::routing::{get, post};
    Router::new()
        .route_as(
            "/{ns}/{repo}/labels",
            Access::Resource,
            get(labels).post(create_label),
        )
        .route_as(
            "/{ns}/{repo}/labels/{id}",
            Access::Resource,
            post(update_label),
        )
        .route_as(
            "/{ns}/{repo}/labels/{id}/delete",
            Access::Resource,
            post(delete_label),
        )
        .route_as(
            "/{ns}/{repo}/milestones",
            Access::Resource,
            get(milestones).post(create_milestone),
        )
        .route_as(
            "/{ns}/{repo}/milestones/{id}",
            Access::Resource,
            post(update_milestone),
        )
        .route_as(
            "/{ns}/{repo}/milestones/{id}/delete",
            Access::Resource,
            post(delete_milestone),
        )
        .route_as(
            "/{ns}/{repo}/issues/{n}/triage/{what}",
            Access::Resource,
            post(triage),
        )
}
