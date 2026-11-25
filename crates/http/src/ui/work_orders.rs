// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Work orders in the web UI: **Delegate to a bot** on an issue or a
//! merge request (`…/delegate`), the order's page (`…/work-orders/{n}`)
//! with its merge requests and events, and **Stop order**. Everything
//! works without JavaScript.

use askama::Template;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Redirect, Response};
use axum::{Form, Router};
use serde::Deserialize;
use slath_core::authz::Subject;
use slath_service::work_orders::{NewOrder, TargetRef};
use slath_service::{FieldError, ServiceError};

use super::repos::RepoHeader;
use super::settings::error_for;
use super::{PageCtx, Time, Who, render};
use crate::AppState;
use crate::auth::{MaybeSession, RequireSession};
use crate::error::AppError;
use crate::routes::{Access, RouteExt};
use slath_service::repos::RepoAccess;

/// An order's end reason, or `open`, as the pages say it.
fn state_text(o: &slath_db::work_orders::WorkOrder) -> &'static str {
    match o.end_reason.as_deref() {
        None => "Open",
        Some("merged") => "Merged",
        Some("closed") => "Closed",
        Some("expired") => "Expired",
        Some("stopped") => "Stopped",
        Some(_) => "Ended",
    }
}

/// The details column's work order section of an issue or merge request:
/// its newest order, or the link to delegate it.
pub struct Side {
    /// The order's page, number, bot, and state.
    pub order: Option<(String, i32, Who, &'static str)>,
    /// Opens the delegate form, for those who may (no open order, the
    /// target open, and a bot that could take it).
    pub delegate: Option<String>,
}

/// The section for issue or merge request `target` (`kind` is `issues` or
/// `merge-requests`), or `None` when there's nothing to show.
pub async fn side(
    state: &AppState,
    subject: &Subject,
    a: &RepoAccess,
    (kind, number, id): (&str, i32, uuid::Uuid),
    open: bool,
) -> Result<Option<Side>, AppError> {
    let (issue, mr) = if kind == "issues" {
        (Some(id), None)
    } else {
        (None, Some(id))
    };
    let latest = state.services.latest_work_order(a, issue, mr).await?;
    let (ns, repo) = (&a.repo.namespace, &a.repo.name);
    let busy = latest.as_ref().is_some_and(|o| o.order.state == "open");
    let delegate = (open && !busy && state.services.may_delegate(subject, a).await?)
        .then(|| format!("/{ns}/{repo}/{kind}/{number}/delegate"));
    let order = latest.map(|v| {
        (
            format!("/{ns}/{repo}/work-orders/{}", v.order.number),
            v.order.number,
            Who::account(Some(v.bot.clone())),
            state_text(&v.order),
        )
    });
    Ok((order.is_some() || delegate.is_some()).then_some(Side { order, delegate }))
}

#[derive(Template)]
#[template(path = "repo/work_order.html")]
struct OrderPage {
    ctx: PageCtx,
    header: RepoHeader,
    number: i32,
    state: &'static str,
    /// The target's link and words ("issue #3: Fix the parser").
    target: (String, String),
    bot: Who,
    creator: Who,
    created: Time,
    prefix: String,
    goal: String,
    instructions: Option<String>,
    scope: Vec<String>,
    required_checks: Vec<String>,
    reviewers: Vec<String>,
    /// Used and allowed: revisions, open merge requests.
    revisions: (i64, i32),
    open_mrs: (usize, i32),
    ends: Time,
    ended: Option<(Time, Option<Who>)>,
    /// Each merge request's link, number, title, and state.
    mrs: Vec<(String, i32, String, String)>,
    /// Each event's time, action, decision, rule, target, and actor.
    events: Vec<(Time, String, String, String, String, Who)>,
    /// Where Stop posts, for those who may stop it.
    stop: Option<String>,
}

async fn page(
    State(state): State<AppState>,
    ctx: PageCtx,
    session: MaybeSession,
    Path((ns, repo, n)): Path<(String, String, String)>,
) -> Result<Response, AppError> {
    let n: i32 = n.parse().map_err(|_| AppError::not_found())?;
    let subject = session.subject();
    let (a, v, mrs, revisions) = state
        .services
        .work_order_page(&subject, (&ns, &repo), n)
        .await?;
    let events = state
        .services
        .work_order_events(&subject, (&ns, &repo), n, 0, 200)
        .await?;
    let o = &v.order;
    let target = if o.issue_id.is_some() {
        (
            format!("/{ns}/{repo}/issues/{}", v.target_number),
            format!("issue #{}: {}", v.target_number, v.target_title),
        )
    } else {
        (
            format!("/{ns}/{repo}/merge-requests/{}", v.target_number),
            format!("merge request !{}: {}", v.target_number, v.target_title),
        )
    };
    let open = o.state == "open";
    let may_stop = open
        && matches!(&subject, Subject::User { .. })
        && state.services.may_stop_work_order(&subject, &a, &v).await;
    let page = OrderPage {
        header: RepoHeader::load(&state, &a, &subject, "issues").await,
        number: o.number,
        state: state_text(o),
        target,
        bot: Who::account(Some(v.bot.clone())),
        creator: Who::account(v.creator.clone()),
        created: o.created_at.into(),
        prefix: v.branch_prefix(),
        goal: slath_markup::reveal_bidi(&o.goal).into_owned(),
        instructions: o
            .instructions
            .as_deref()
            .map(|i| slath_markup::reveal_bidi(i).into_owned()),
        scope: o.scope.clone(),
        required_checks: o.required_checks.clone(),
        reviewers: v.reviewers.clone(),
        revisions: (revisions, o.max_revisions),
        open_mrs: (
            mrs.iter().filter(|m| m.state == "open").count(),
            o.max_open_mrs,
        ),
        ends: o.expires_at.into(),
        ended: o
            .ended_at
            .map(|t| (t.into(), v.ended_by.clone().map(|n| Who::account(Some(n))))),
        mrs: mrs
            .iter()
            .map(|m| {
                (
                    format!("/{ns}/{repo}/merge-requests/{}", m.number),
                    m.number,
                    m.title.clone(),
                    m.state.clone(),
                )
            })
            .collect(),
        events: events
            .into_iter()
            .map(|(e, actor)| {
                (
                    e.created_at.into(),
                    e.action,
                    if e.decision == "allowed" {
                        "Allowed".to_owned()
                    } else {
                        "Denied".to_owned()
                    },
                    e.policy.unwrap_or_default(),
                    e.target.unwrap_or_default(),
                    Who::account(actor),
                )
            })
            .collect(),
        stop: may_stop.then(|| format!("/{ns}/{repo}/work-orders/{n}/stop")),
        ctx,
    };
    Ok(render(&page))
}

async fn stop(
    State(state): State<AppState>,
    RequireSession(s): RequireSession,
    Path((ns, repo, n)): Path<(String, String, String)>,
) -> Result<Response, AppError> {
    let n: i32 = n.parse().map_err(|_| AppError::not_found())?;
    state
        .services
        .stop_work_order(&s.subject, (&ns, &repo), n)
        .await?;
    Ok(Redirect::to(&format!("/{ns}/{repo}/work-orders/{n}")).into_response())
}

#[derive(Template)]
#[template(path = "repo/work_order_delegate.html")]
struct DelegatePage {
    ctx: PageCtx,
    header: RepoHeader,
    /// "issue #3" or "merge request !4", and its title.
    target: String,
    title: String,
    action: String,
    back: String,
    bots: Vec<String>,
    f: DelegateForm,
    /// The limits group opens when one of its fields has an error.
    limits_open: bool,
    errors: Vec<FieldError>,
}

impl DelegatePage {
    fn error(&self, field: &str) -> Option<&str> {
        error_for(&self.errors, field)
    }
}

/// The delegate form, every field as typed.
#[derive(Deserialize, Default, Clone)]
struct DelegateForm {
    #[serde(default)]
    bot: String,
    #[serde(default)]
    goal: String,
    #[serde(default)]
    instructions: String,
    /// One path glob per line.
    #[serde(default)]
    scope: String,
    #[serde(default)]
    max_revisions: String,
    #[serde(default)]
    max_open_mrs: String,
    #[serde(default)]
    expires_in_days: String,
    /// One status context per line.
    #[serde(default)]
    required_checks: String,
    /// Names, separated by commas.
    #[serde(default)]
    reviewers: String,
}

const LIMIT_FIELDS: &[&str] = &[
    "scope",
    "max_revisions",
    "max_open_mrs",
    "expires_in_days",
    "required_checks",
    "reviewers",
];

fn lines(s: &str) -> Vec<String> {
    s.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_owned)
        .collect()
}

/// A number field: empty is the default; anything else must be a number.
fn number<T: std::str::FromStr>(field: &str, v: &str, errors: &mut Vec<FieldError>) -> Option<T> {
    let v = v.trim();
    if v.is_empty() {
        return None;
    }
    v.parse().map_or_else(
        |_| {
            errors.push(FieldError::new(field, "invalid", "Enter a whole number."));
            None
        },
        Some,
    )
}

/// The target from the path: `issues` or `merge-requests`, and its number.
fn target_of(kind: &str, n: &str) -> Result<TargetRef, AppError> {
    let n: i32 = n.parse().map_err(|_| AppError::not_found())?;
    match kind {
        "issues" => Ok(TargetRef::Issue(n)),
        "merge-requests" => Ok(TargetRef::MergeRequest(n)),
        _ => Err(AppError::not_found()),
    }
}

async fn delegate_page(
    state: &AppState,
    ctx: PageCtx,
    subject: &Subject,
    (ns, repo, kind, target): (&str, &str, &str, TargetRef),
    form: Option<(DelegateForm, Vec<FieldError>)>,
) -> Result<Response, AppError> {
    let (a, label, title) = match target {
        TargetRef::Issue(n) => {
            let (a, i) = state.services.issue(subject, ns, repo, n).await?;
            (a, format!("issue #{n}"), i.title)
        }
        TargetRef::MergeRequest(n) => {
            let (a, m) = state.services.mr(subject, ns, repo, n).await?;
            (a, format!("merge request !{n}"), m.title)
        }
    };
    if !state.services.may_delegate(subject, &a).await? {
        return Err(AppError::not_found());
    }
    let bots = state.services.order_bots(&a).await?;
    let n = match target {
        TargetRef::Issue(n) | TargetRef::MergeRequest(n) => n,
    };
    let back = format!("/{ns}/{repo}/{kind}/{n}");
    let (f, errors) = form.unwrap_or_else(|| {
        (
            DelegateForm {
                bot: bots.first().cloned().unwrap_or_default(),
                goal: title.clone(),
                ..DelegateForm::default()
            },
            Vec::new(),
        )
    });
    let limits_open = errors
        .iter()
        .any(|e| LIMIT_FIELDS.contains(&e.field.as_str()));
    let status = if errors.is_empty() {
        StatusCode::OK
    } else {
        StatusCode::UNPROCESSABLE_ENTITY
    };
    let mut res = render(&DelegatePage {
        header: RepoHeader::load(
            state,
            &a,
            subject,
            if kind == "issues" {
                "issues"
            } else {
                "merge-requests"
            },
        )
        .await,
        target: label,
        title,
        action: format!("{back}/delegate"),
        back,
        bots,
        f,
        limits_open,
        errors,
        ctx,
    });
    *res.status_mut() = status;
    Ok(res)
}

async fn show_delegate(
    state: &AppState,
    ctx: PageCtx,
    s: &slath_service::sessions::Session,
    (ns, repo, kind, n): (&str, &str, &str, &str),
) -> Result<Response, AppError> {
    let target = target_of(kind, n)?;
    delegate_page(state, ctx, &s.subject, (ns, repo, kind, target), None).await
}

async fn show_issue_delegate(
    State(state): State<AppState>,
    ctx: PageCtx,
    RequireSession(s): RequireSession,
    Path((ns, repo, n)): Path<(String, String, String)>,
) -> Result<Response, AppError> {
    show_delegate(&state, ctx, &s, (&ns, &repo, "issues", &n)).await
}

async fn show_mr_delegate(
    State(state): State<AppState>,
    ctx: PageCtx,
    RequireSession(s): RequireSession,
    Path((ns, repo, n)): Path<(String, String, String)>,
) -> Result<Response, AppError> {
    show_delegate(&state, ctx, &s, (&ns, &repo, "merge-requests", &n)).await
}

async fn issue_delegate(
    State(state): State<AppState>,
    ctx: PageCtx,
    RequireSession(s): RequireSession,
    Path((ns, repo, n)): Path<(String, String, String)>,
    Form(f): Form<DelegateForm>,
) -> Result<Response, AppError> {
    delegate(state, ctx, s, (ns, repo, "issues".to_owned(), n), f).await
}

async fn mr_delegate(
    State(state): State<AppState>,
    ctx: PageCtx,
    RequireSession(s): RequireSession,
    Path((ns, repo, n)): Path<(String, String, String)>,
    Form(f): Form<DelegateForm>,
) -> Result<Response, AppError> {
    delegate(state, ctx, s, (ns, repo, "merge-requests".to_owned(), n), f).await
}

async fn delegate(
    state: AppState,
    ctx: PageCtx,
    s: slath_service::sessions::Session,
    (ns, repo, kind, n): (String, String, String, String),
    f: DelegateForm,
) -> Result<Response, AppError> {
    let target = target_of(&kind, &n)?;
    let mut errors = Vec::new();
    let order = NewOrder {
        target,
        bot: f.bot.clone(),
        goal: f.goal.clone(),
        instructions: Some(f.instructions.clone()),
        scope: lines(&f.scope),
        max_revisions: number("max_revisions", &f.max_revisions, &mut errors),
        max_open_mrs: number("max_open_mrs", &f.max_open_mrs, &mut errors),
        expires_in_days: number("expires_in_days", &f.expires_in_days, &mut errors),
        required_checks: lines(&f.required_checks),
        reviewers: f
            .reviewers
            .split(',')
            .map(|r| r.trim().trim_start_matches('@').to_owned())
            .filter(|r| !r.is_empty())
            .collect(),
    };
    let result = if errors.is_empty() {
        state
            .services
            .create_work_order(&s.subject, (&ns, &repo), &order)
            .await
    } else {
        Err(ServiceError::Invalid(errors))
    };
    match result {
        Ok(v) => Ok(
            Redirect::to(&format!("/{ns}/{repo}/work-orders/{}", v.order.number)).into_response(),
        ),
        Err(ServiceError::Invalid(errors)) => {
            // The service names fields by JSON pointer; the form by id.
            let errors = errors
                .into_iter()
                .map(|e| FieldError {
                    field: e.field.trim_start_matches('/').to_owned(),
                    ..e
                })
                .collect();
            delegate_page(
                &state,
                ctx,
                &s.subject,
                (&ns, &repo, &kind, target),
                Some((f, errors)),
            )
            .await
        }
        Err(ServiceError::Conflict(why)) => {
            let errors = vec![FieldError::new("bot", "conflict", why)];
            delegate_page(
                &state,
                ctx,
                &s.subject,
                (&ns, &repo, &kind, target),
                Some((f, errors)),
            )
            .await
        }
        Err(e) => Err(e.into()),
    }
}

pub fn router() -> Router<AppState> {
    Router::new()
        .route_as(
            "/{ns}/{repo}/issues/{n}/delegate",
            Access::Resource,
            axum::routing::get(show_issue_delegate).post(issue_delegate),
        )
        .route_as(
            "/{ns}/{repo}/merge-requests/{n}/delegate",
            Access::Resource,
            axum::routing::get(show_mr_delegate).post(mr_delegate),
        )
        .route_as(
            "/{ns}/{repo}/work-orders/{n}",
            Access::Resource,
            axum::routing::get(page),
        )
        .route_as(
            "/{ns}/{repo}/work-orders/{n}/stop",
            Access::Resource,
            axum::routing::post(stop),
        )
}
