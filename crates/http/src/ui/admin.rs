// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Site administration pages (`/-/admin/…`) and the organization audit log.
//! Everyone but site administrators gets "not found".

use askama::Template;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Redirect, Response};
use axum::{Form, Router};
use serde::Deserialize;
use slath_core::Secret;
use slath_core::authz::{RepoState, Subject, UserState};
use slath_service::{FieldError, ServiceError};

use super::settings::error_for;
use super::{PageCtx, Time, render};
use crate::AppState;
use crate::auth::{MaybeSession, Sudo};
use crate::error::AppError;

const PER_PAGE: usize = 100;

#[derive(Deserialize, Default)]
struct ListQuery {
    status: Option<String>,
}

fn status_message(s: Option<&str>) -> Option<&'static str> {
    Some(match s? {
        "created" => "User created.",
        "suspended" => "User suspended. Their sessions and tokens were revoked.",
        "unsuspended" => "User reactivated.",
        "admin-granted" => "Site administration granted.",
        "admin-revoked" => "Site administration revoked.",
        "restored" => "Repository restored.",
        _ => return None,
    })
}

struct UserRow {
    name: String,
    display_name: Option<String>,
    email: String,
    admin: bool,
    suspended: bool,
    created: Time,
}

#[derive(Template)]
#[template(path = "admin/users.html")]
struct UsersPage<'a> {
    ctx: PageCtx,
    section: &'static str,
    users: Vec<UserRow>,
    message: Option<&'a str>,
    errors: Vec<FieldError>,
    username: String,
    email: String,
    password: Option<String>,
}

impl UsersPage<'_> {
    fn error(&self, field: &str) -> Option<&str> {
        error_for(&self.errors, field)
    }
}

/// The create-user form's state, and the generated password after success.
#[derive(Default)]
struct Draft {
    errors: Vec<FieldError>,
    username: String,
    email: String,
    password: Option<String>,
}

async fn users_page(
    state: &AppState,
    ctx: PageCtx,
    subject: &Subject,
    message: Option<&str>,
    draft: Draft,
) -> Result<Response, AppError> {
    let Draft {
        errors,
        username,
        email,
        password,
    } = draft;
    let users = state
        .services
        .admin_users(subject, 0, 1000)
        .await?
        .into_iter()
        .map(|u| UserRow {
            suspended: u.state == UserState::Suspended,
            name: u.name,
            display_name: u.display_name,
            email: u.email.unwrap_or_default(),
            admin: u.is_admin,
            created: u.created_at.into(),
        })
        .collect();
    let status = if errors.is_empty() {
        StatusCode::OK
    } else {
        StatusCode::UNPROCESSABLE_ENTITY
    };
    let mut res = render(&UsersPage {
        ctx,
        section: "users",
        users,
        message,
        errors,
        username,
        email,
        password,
    });
    *res.status_mut() = status;
    Ok(res)
}

async fn users(
    State(state): State<AppState>,
    ctx: PageCtx,
    session: MaybeSession,
    Query(q): Query<ListQuery>,
) -> Result<Response, AppError> {
    users_page(
        &state,
        ctx,
        &session.subject(),
        status_message(q.status.as_deref()),
        Draft::default(),
    )
    .await
}

#[derive(Deserialize)]
struct NewUserForm {
    #[serde(default)]
    username: String,
    #[serde(default)]
    email: String,
    #[serde(default)]
    admin: Option<String>,
}

async fn create_user(
    State(state): State<AppState>,
    ctx: PageCtx,
    Sudo(s): Sudo,
    Form(f): Form<NewUserForm>,
) -> Result<Response, AppError> {
    state.services.require_site_admin(&s.subject)?;
    // The administrator hands this one-time password to the user.
    let bytes = slath_auth::token::random_bytes::<18>().map_err(|e| AppError::internal(&e))?;
    let password = crate::auth::b64url(&bytes);
    let result = state
        .services
        .create_account(
            &s.subject,
            slath_service::accounts::NewAccount {
                username: f.username.clone(),
                email: (!f.email.trim().is_empty()).then(|| f.email.clone()),
                display_name: None,
                password: Some(Secret::new(password.clone())),
                is_admin: f.admin.is_some(),
            },
        )
        .await;
    match result {
        Ok(_) => {
            let mut res = users_page(
                &state,
                ctx,
                &s.subject,
                Some("User created. Give them this password; it won't be shown again."),
                Draft {
                    password: Some(password),
                    ..Draft::default()
                },
            )
            .await?;
            res.headers_mut().insert(
                axum::http::header::CACHE_CONTROL,
                axum::http::HeaderValue::from_static("no-store"),
            );
            Ok(res)
        }
        Err(ServiceError::Invalid(errors)) => {
            let draft = Draft {
                errors,
                username: f.username,
                email: f.email,
                password: None,
            };
            users_page(&state, ctx, &s.subject, None, draft).await
        }
        Err(e) => Err(e.into()),
    }
}

async fn user_action(
    State(state): State<AppState>,
    Sudo(s): Sudo,
    Path((name, op)): Path<(String, String)>,
) -> Result<Response, AppError> {
    let svc = &state.services;
    let status = match op.as_str() {
        "suspend" => svc
            .admin_set_suspended(&s.subject, true, &name, true)
            .await
            .map(|()| "suspended"),
        "unsuspend" => svc
            .admin_set_suspended(&s.subject, true, &name, false)
            .await
            .map(|()| "unsuspended"),
        "grant-admin" => svc
            .admin_set_admin(&s.subject, true, &name, true)
            .await
            .map(|()| "admin-granted"),
        "revoke-admin" => svc
            .admin_set_admin(&s.subject, true, &name, false)
            .await
            .map(|()| "admin-revoked"),
        _ => return Err(AppError::not_found()),
    }?;
    Ok(Redirect::to(&format!("/-/admin/users?status={status}")).into_response())
}

struct OrgRow {
    name: String,
    display_name: Option<String>,
    created: Time,
}

#[derive(Template)]
#[template(path = "admin/orgs.html")]
struct OrgsPage {
    ctx: PageCtx,
    section: &'static str,
    orgs: Vec<OrgRow>,
}

async fn orgs(
    State(state): State<AppState>,
    ctx: PageCtx,
    session: MaybeSession,
) -> Result<Response, AppError> {
    let orgs = state
        .services
        .admin_orgs(&session.subject(), 0, 1000)
        .await?
        .into_iter()
        .map(|o| OrgRow {
            name: o.name,
            display_name: o.display_name,
            created: o.created_at.into(),
        })
        .collect();
    Ok(render(&OrgsPage {
        ctx,
        section: "orgs",
        orgs,
    }))
}

struct RepoRow {
    id: String,
    full_name: String,
    visibility: &'static str,
    state: &'static str,
    deleted: Option<Time>,
    size: i64,
}

#[derive(Template)]
#[template(path = "admin/repos.html")]
struct ReposPage<'a> {
    ctx: PageCtx,
    section: &'static str,
    repos: Vec<RepoRow>,
    message: Option<&'a str>,
}

async fn repos(
    State(state): State<AppState>,
    ctx: PageCtx,
    session: MaybeSession,
    Query(q): Query<ListQuery>,
) -> Result<Response, AppError> {
    let repos = state
        .services
        .admin_repos(&session.subject(), 0, 1000)
        .await?
        .into_iter()
        .map(|r| RepoRow {
            id: r.id.to_string(),
            full_name: format!("{}/{}", r.namespace, r.name),
            visibility: slath_db::repos::visibility_str(r.visibility),
            state: match r.state {
                RepoState::Active => "Active",
                RepoState::Archived => "Archived",
                RepoState::Deleting => "Deleted",
            },
            deleted: r.deleted_at.map(Into::into),
            size: r.size_bytes,
        })
        .collect();
    Ok(render(&ReposPage {
        ctx,
        section: "repos",
        repos,
        message: status_message(q.status.as_deref()),
    }))
}

async fn restore(
    State(state): State<AppState>,
    Sudo(s): Sudo,
    Path(id): Path<String>,
) -> Result<Response, AppError> {
    state.services.require_site_admin(&s.subject)?;
    let id = id.parse().map_err(|_| AppError::not_found())?;
    state.services.restore_repo(&s.subject, id).await?;
    Ok(Redirect::to("/-/admin/repos?status=restored").into_response())
}

struct EventRow {
    time: Time,
    actor: String,
    ip: String,
    action: String,
    target: String,
    data: String,
}

#[derive(Template)]
#[template(path = "admin/audit.html")]
struct AuditPage {
    ctx: PageCtx,
    section: &'static str,
    events: Vec<EventRow>,
    action: String,
    actor: String,
    /// Set for an organization's audit log.
    org: Option<String>,
    page: usize,
    next: bool,
}

#[derive(Deserialize, Default)]
struct AuditQuery {
    action: Option<String>,
    actor: Option<String>,
    page: Option<usize>,
}

async fn audit_page(
    state: &AppState,
    ctx: PageCtx,
    subject: &Subject,
    q: AuditQuery,
    org: Option<(String, uuid::Uuid)>,
) -> Result<Response, AppError> {
    let page = q.page.unwrap_or(1).clamp(1, 1000);
    let filter = slath_db::audit::Filter {
        action: q.action.clone().filter(|a| !a.trim().is_empty()),
        actor: q.actor.clone().filter(|a| !a.trim().is_empty()),
        org_id: org.as_ref().map(|(_, id)| *id),
    };
    let mut events = state
        .services
        .audit_events(subject, &filter, (page - 1) * PER_PAGE, PER_PAGE + 1)
        .await?;
    let next = events.len() > PER_PAGE;
    events.truncate(PER_PAGE);
    Ok(render(&AuditPage {
        ctx,
        section: "audit",
        events: events
            .into_iter()
            .map(|e| EventRow {
                time: e.created_at.into(),
                actor: e.actor.unwrap_or_else(|| "—".to_owned()),
                ip: e.actor_ip.unwrap_or_default(),
                action: e.action,
                target: format!(
                    "{}{}",
                    e.target_kind,
                    e.target_id.map(|t| format!(" {t}")).unwrap_or_default()
                ),
                data: if e.data.as_object().is_some_and(serde_json::Map::is_empty) {
                    String::new()
                } else {
                    e.data.to_string()
                },
            })
            .collect(),
        action: q.action.unwrap_or_default(),
        actor: q.actor.unwrap_or_default(),
        org: org.map(|(n, _)| n),
        page,
        next,
    }))
}

async fn audit(
    State(state): State<AppState>,
    ctx: PageCtx,
    session: MaybeSession,
    Query(q): Query<AuditQuery>,
) -> Result<Response, AppError> {
    audit_page(&state, ctx, &session.subject(), q, None).await
}

async fn org_audit(
    State(state): State<AppState>,
    ctx: PageCtx,
    session: MaybeSession,
    Path(org): Path<String>,
    Query(q): Query<AuditQuery>,
) -> Result<Response, AppError> {
    let Some((name, slath_db::orgs::NamespaceKind::Org(id))) =
        state.services.resolve_namespace(&org).await?
    else {
        return Err(AppError::not_found());
    };
    audit_page(
        &state,
        ctx,
        &session.subject(),
        q,
        Some((name, *id.as_uuid())),
    )
    .await
}

#[derive(Template)]
#[template(path = "admin/health.html")]
struct HealthPage {
    ctx: PageCtx,
    section: &'static str,
    git: String,
    git_ok: bool,
    schema: i32,
    required_schema: i32,
    version: &'static str,
    jobs: Vec<(String, String, i64)>,
}

async fn health(
    State(state): State<AppState>,
    ctx: PageCtx,
    session: MaybeSession,
) -> Result<Response, AppError> {
    let h = state.services.health(&session.subject()).await?;
    Ok(render(&HealthPage {
        ctx,
        section: "health",
        git_ok: h.git_version.is_ok(),
        git: h.git_version.unwrap_or_else(|e| e),
        schema: h.schema,
        required_schema: h.required_schema,
        version: env!("CARGO_PKG_VERSION"),
        jobs: h.jobs,
    }))
}

async fn index(State(state): State<AppState>, session: MaybeSession) -> Result<Redirect, AppError> {
    state.services.require_site_admin(&session.subject())?;
    Ok(Redirect::to("/-/admin/users"))
}

pub fn router() -> Router<AppState> {
    use axum::routing::{get, post};
    Router::new()
        .route("/-/admin", get(index))
        .route("/-/admin/users", get(users).post(create_user))
        .route("/-/admin/users/{name}/{op}", post(user_action))
        .route("/-/admin/orgs", get(orgs))
        .route("/-/admin/repos", get(repos))
        .route("/-/admin/repos/{id}/restore", post(restore))
        .route("/-/admin/audit", get(audit))
        .route("/-/admin/health", get(health))
        .route("/{org}/-/audit", get(org_audit))
}
