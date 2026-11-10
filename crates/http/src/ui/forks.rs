// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Forking: the fork form and the list of a repository's forks.

use askama::Template;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Redirect, Response};
use axum::{Form, Router};
use serde::Deserialize;
use slath_core::authz::{Action, Visibility};
use slath_service::forks::{BranchCompare, ForkInput, Synced};
use slath_service::{FieldError, ServiceError};

use super::repos::{RepoHeader, parse_visibility, visibility_name};
use super::settings::error_for;
use super::{PageCtx, Time, render};
use crate::AppState;
use crate::auth::{MaybeSession, RequireSession, Sudo};
use crate::error::AppError;
use crate::routes::{Access, RouteExt};

/// The highest visibility a form offers: 0 private, 1 internal, 2 public.
pub(super) const fn visibility_rank(v: Visibility) -> u8 {
    match v {
        Visibility::Private => 0,
        Visibility::Internal => 1,
        Visibility::Public => 2,
    }
}

#[derive(Template)]
#[template(path = "repo/fork.html")]
struct ForkPage {
    ctx: PageCtx,
    header: RepoHeader,
    namespaces: Vec<String>,
    selected: String,
    name: String,
    visibility: &'static str,
    max_visibility: u8,
    default_branch: String,
    default_branch_only: bool,
    forks: usize,
    errors: Vec<FieldError>,
}

impl ForkPage {
    fn error(&self, field: &str) -> Option<&str> {
        error_for(&self.errors, field)
    }
}

async fn fork_page(
    State(state): State<AppState>,
    ctx: PageCtx,
    RequireSession(s): RequireSession,
    Path((ns, repo)): Path<(String, String)>,
) -> Result<Response, AppError> {
    let a = state
        .services
        .repo_access(&s.subject, &ns, &repo, Action::RepoFork)
        .await?;
    let forks = state.services.list_forks(&s.subject, &a).await?.len();
    let namespaces = state.services.fork_targets(&s.subject, &a).await?;
    Ok(render(&ForkPage {
        header: RepoHeader::load(&state, &a, &s.subject, "fork").await,
        selected: namespaces.first().cloned().unwrap_or_default(),
        namespaces,
        name: a.repo.name.clone(),
        visibility: visibility_name(a.repo.visibility),
        max_visibility: visibility_rank(a.repo.visibility),
        default_branch: a.repo.default_branch.clone(),
        default_branch_only: false,
        forks,
        errors: Vec::new(),
        ctx,
    }))
}

#[derive(Deserialize)]
struct ForkForm {
    #[serde(default)]
    namespace: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    visibility: String,
    #[serde(default)]
    default_branch_only: Option<String>,
}

async fn fork_submit(
    State(state): State<AppState>,
    ctx: PageCtx,
    RequireSession(s): RequireSession,
    Path((ns, repo)): Path<(String, String)>,
    Form(f): Form<ForkForm>,
) -> Result<Response, AppError> {
    let visibility = parse_visibility(&f.visibility);
    let only = f.default_branch_only.is_some();
    let input = ForkInput {
        namespace: f.namespace.clone(),
        name: f.name.clone(),
        visibility,
        default_branch_only: only,
    };
    match state
        .services
        .fork_repo(&s.subject, &ns, &repo, input)
        .await
    {
        Ok(a) => {
            Ok(Redirect::to(&format!("/{}/{}", a.repo.namespace, a.repo.name)).into_response())
        }
        Err(ServiceError::Invalid(errors)) => {
            let a = state
                .services
                .repo_access(&s.subject, &ns, &repo, Action::RepoFork)
                .await?;
            let forks = state.services.list_forks(&s.subject, &a).await?.len();
            let mut res = render(&ForkPage {
                header: RepoHeader::load(&state, &a, &s.subject, "fork").await,
                namespaces: state.services.fork_targets(&s.subject, &a).await?,
                selected: f.namespace,
                name: f.name,
                visibility: visibility_name(visibility.unwrap_or(a.repo.visibility)),
                max_visibility: visibility_rank(a.repo.visibility),
                default_branch: a.repo.default_branch.clone(),
                default_branch_only: only,
                forks,
                errors,
                ctx,
            });
            *res.status_mut() = StatusCode::UNPROCESSABLE_ENTITY;
            Ok(res)
        }
        Err(e) => Err(e.into()),
    }
}

struct ForkRow {
    full_name: String,
    description: Option<String>,
    visibility: &'static str,
    created: Time,
}

#[derive(Template)]
#[template(path = "repo/forks.html")]
struct ForksPage {
    ctx: PageCtx,
    header: RepoHeader,
    rows: Vec<ForkRow>,
}

async fn forks_page(
    State(state): State<AppState>,
    ctx: PageCtx,
    session: MaybeSession,
    Path((ns, repo)): Path<(String, String)>,
) -> Result<Response, AppError> {
    let subject = session.subject();
    let a = state
        .services
        .repo_access(&subject, &ns, &repo, Action::RepoRead)
        .await?;
    let rows = state
        .services
        .list_forks(&subject, &a)
        .await?
        .into_iter()
        .map(|r| ForkRow {
            full_name: format!("{}/{}", r.namespace, r.name),
            description: r.description,
            visibility: visibility_name(r.visibility),
            created: r.created_at.into(),
        })
        .collect();
    Ok(render(&ForksPage {
        header: RepoHeader::load(&state, &a, &subject, "forks").await,
        rows,
        ctx,
    }))
}

#[derive(Deserialize)]
struct SyncForm {
    #[serde(default)]
    branch: String,
}

/// `POST /{ns}/{repo}/sync`: a fast-forward (or the new branch); a
/// diverged branch goes to the page that explains the choice.
async fn sync(
    State(state): State<AppState>,
    RequireSession(s): RequireSession,
    Path((ns, repo)): Path<(String, String)>,
    Form(f): Form<SyncForm>,
) -> Result<Response, AppError> {
    match state
        .services
        .sync_fork(&s.subject, (&ns, &repo), &f.branch, false, false)
        .await?
    {
        Synced::Diverged { .. } => Ok(Redirect::to(&format!(
            "/{ns}/{repo}/sync?branch={}",
            percent_encoding::utf8_percent_encode(&f.branch, percent_encoding::NON_ALPHANUMERIC)
        ))
        .into_response()),
        _ => Ok(Redirect::to(&format!("/{ns}/{repo}")).into_response()),
    }
}

#[derive(Template)]
#[template(path = "repo/sync.html")]
struct SyncPage {
    ctx: PageCtx,
    header: RepoHeader,
    compare: Option<BranchCompare>,
}

/// `GET /{ns}/{repo}/sync?branch=`: how the branch compares, and for a
/// diverged branch the way to drop the fork's commits.
async fn sync_page(
    State(state): State<AppState>,
    ctx: PageCtx,
    RequireSession(s): RequireSession,
    Path((ns, repo)): Path<(String, String)>,
    axum::extract::Query(q): axum::extract::Query<SyncForm>,
) -> Result<Response, AppError> {
    let a = state
        .services
        .repo_access(&s.subject, &ns, &repo, Action::RepoPush)
        .await?;
    let branch = if q.branch.is_empty() {
        a.repo.default_branch.clone()
    } else {
        q.branch
    };
    let compare = state.services.fork_compare(&s.subject, &a, &branch).await?;
    Ok(render(&SyncPage {
        header: RepoHeader::load(&state, &a, &s.subject, "fork").await,
        compare,
        ctx,
    }))
}

/// `POST /{ns}/{repo}/sync/discard` (sudo): drops the fork's own commits.
async fn sync_discard(
    State(state): State<AppState>,
    Sudo(s): Sudo,
    Path((ns, repo)): Path<(String, String)>,
    Form(f): Form<SyncForm>,
) -> Result<Response, AppError> {
    state
        .services
        .sync_fork(&s.subject, (&ns, &repo), &f.branch, true, true)
        .await?;
    Ok(Redirect::to(&format!("/{ns}/{repo}")).into_response())
}

pub fn router() -> Router<AppState> {
    use axum::routing::get;
    Router::new()
        .route_as(
            "/{ns}/{repo}/fork",
            Access::Resource,
            get(fork_page).post(fork_submit),
        )
        .route_as("/{ns}/{repo}/forks", Access::Resource, get(forks_page))
        .route_as(
            "/{ns}/{repo}/sync",
            Access::Resource,
            get(sync_page).post(sync),
        )
        .route_as(
            "/{ns}/{repo}/sync/discard",
            Access::Resource,
            axum::routing::post(sync_discard),
        )
}
