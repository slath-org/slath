// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Search: the `/-/search` page and `/api/v1/search/{issues,repos}`.

use askama::Template;
use axum::Router;
use axum::extract::{OriginalUri, Query, State};
use axum::response::Response;
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};

use super::{PageCtx, Time, render};
use crate::AppState;
use crate::api::ApiSubject;
use crate::api::support::{PageParams, page};
use crate::auth::MaybeSession;
use crate::error::AppError;
use crate::routes::{Access, RouteExt};

const PER_PAGE: i64 = 30;

struct IssueRow {
    href: String,
    repo: String,
    number: i32,
    title: String,
    open: bool,
    updated: Time,
}

struct RepoRow {
    full: String,
    description: Option<String>,
    private: bool,
}

#[derive(Template)]
#[template(path = "search.html")]
struct SearchPage {
    ctx: PageCtx,
    q: String,
    /// `issues` or `repos`.
    kind: String,
    /// `owner/repo` when searching one repository.
    repo: String,
    issues: Vec<IssueRow>,
    repos: Vec<RepoRow>,
    next: Option<String>,
    searched: bool,
}

#[derive(Deserialize, Default)]
struct SearchQuery {
    #[serde(default)]
    q: String,
    #[serde(default, rename = "type")]
    kind: String,
    #[serde(default)]
    repo: String,
    page: Option<i64>,
}

fn enc(v: &str) -> String {
    let mut out = String::new();
    for b in v.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
            out.push(char::from(b));
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// `GET /-/search?q=&type=issues|repos&repo=owner/name`.
async fn search_page(
    State(state): State<AppState>,
    ctx: PageCtx,
    session: MaybeSession,
    Query(q): Query<SearchQuery>,
) -> Result<Response, AppError> {
    let subject = session.subject();
    let kind = if q.kind == "repos" { "repos" } else { "issues" };
    let page_no = q.page.unwrap_or(1).max(1);
    let offset = (page_no - 1) * PER_PAGE;
    let scope = q.repo.split_once('/').filter(|_| kind == "issues");
    let mut issues = Vec::new();
    let mut repos = Vec::new();
    let more;
    if kind == "issues" {
        let hits = state
            .services
            .search_issues(&subject, &q.q, scope, offset, PER_PAGE + 1)
            .await?;
        more = hits.len() as i64 > PER_PAGE;
        issues = hits
            .into_iter()
            .take(PER_PAGE as usize)
            .map(|h| IssueRow {
                href: format!("/{}/issues/{}", h.repo, h.number),
                repo: h.repo,
                number: h.number,
                title: h.title,
                open: h.state == "open",
                updated: h.updated_at.into(),
            })
            .collect();
    } else {
        let hits = state
            .services
            .search_repos(&subject, &q.q, offset, PER_PAGE + 1)
            .await?;
        more = hits.len() as i64 > PER_PAGE;
        repos = hits
            .into_iter()
            .take(PER_PAGE as usize)
            .map(|h| RepoRow {
                full: format!("{}/{}", h.namespace, h.name),
                description: h.description,
                private: h.private,
            })
            .collect();
    }
    let next = more.then(|| {
        format!(
            "/-/search?q={}&type={kind}&repo={}&page={}",
            enc(&q.q),
            enc(&q.repo),
            page_no + 1
        )
    });
    Ok(render(&SearchPage {
        ctx,
        searched: !q.q.trim().is_empty(),
        q: q.q,
        kind: kind.to_owned(),
        repo: if scope.is_some() {
            q.repo
        } else {
            String::new()
        },
        issues,
        repos,
        next,
    }))
}

/// A matching issue.
#[derive(Serialize, ToSchema)]
pub struct IssueHitJson {
    /// `owner/repo`.
    pub repository: String,
    pub number: i32,
    pub title: String,
    pub state: String,
    pub updated_at: String,
}

/// A matching repository.
#[derive(Serialize, ToSchema)]
pub struct RepoHitJson {
    pub namespace: String,
    pub name: String,
    pub description: Option<String>,
    /// `public` or `private`.
    pub visibility: String,
}

#[derive(Serialize, ToSchema)]
pub struct IssueHitPage {
    pub items: Vec<IssueHitJson>,
    pub next_cursor: Option<String>,
}

#[derive(Serialize, ToSchema)]
pub struct RepoHitPage {
    pub items: Vec<RepoHitJson>,
    pub next_cursor: Option<String>,
}

#[derive(Deserialize, IntoParams)]
pub struct IssueSearchQuery {
    /// Search terms: words, `"phrases"`, `-excluded`, `or`.
    #[serde(default)]
    pub q: String,
    /// Limit to one repository (`owner/repo`).
    pub repo: Option<String>,
}

#[derive(Deserialize, IntoParams)]
pub struct RepoSearchQuery {
    /// Part of a repository's name or description.
    #[serde(default)]
    pub q: String,
}

/// Issues (titles, descriptions, and comments) you can read, best match first.
#[utoipa::path(get, path = "/api/v1/search/issues", tag = "issues", security((), ("token" = ["repo:read"])), params(IssueSearchQuery, PageParams),
    responses((status = 200, body = IssueHitPage), (status = 401, body = crate::api::Problem), (status = 404, body = crate::api::Problem)))]
pub async fn api_issues(
    State(state): State<AppState>,
    ApiSubject(s): ApiSubject,
    OriginalUri(uri): OriginalUri,
    Query(q): Query<IssueSearchQuery>,
    Query(p): Query<PageParams>,
) -> Result<Response, AppError> {
    let (offset, limit) = (p.offset(&state)?, p.limit(state.config.limits.api_page_max));
    let scope = match q.repo.as_deref() {
        Some(r) => Some(r.split_once('/').ok_or_else(AppError::not_found)?),
        None => None,
    };
    let hits = state
        .services
        .search_issues(&s, &q.q, scope, offset as i64, limit as i64 + 1)
        .await?;
    let items: Vec<IssueHitJson> = hits
        .into_iter()
        .map(|h| IssueHitJson {
            repository: h.repo,
            number: h.number,
            title: h.title,
            state: h.state,
            updated_at: Time::from(h.updated_at).iso,
        })
        .collect();
    Ok(page(&state, &uri, items, offset, limit))
}

/// Repositories you can see whose name or description contains the query.
#[utoipa::path(get, path = "/api/v1/search/repos", tag = "repos", security((), ("token" = ["repo:read"])), params(RepoSearchQuery, PageParams),
    responses((status = 200, body = RepoHitPage), (status = 401, body = crate::api::Problem)))]
pub async fn api_repos(
    State(state): State<AppState>,
    ApiSubject(s): ApiSubject,
    OriginalUri(uri): OriginalUri,
    Query(q): Query<RepoSearchQuery>,
    Query(p): Query<PageParams>,
) -> Result<Response, AppError> {
    let (offset, limit) = (p.offset(&state)?, p.limit(state.config.limits.api_page_max));
    let hits = state
        .services
        .search_repos(&s, &q.q, offset as i64, limit as i64 + 1)
        .await?;
    let items: Vec<RepoHitJson> = hits
        .into_iter()
        .map(|h| RepoHitJson {
            namespace: h.namespace,
            name: h.name,
            description: h.description,
            visibility: if h.private { "private" } else { "public" }.to_owned(),
        })
        .collect();
    Ok(page(&state, &uri, items, offset, limit))
}

pub fn router() -> Router<AppState> {
    use axum::routing::get;
    Router::new().route_as("/-/search", Access::PublicRead, get(search_page))
}

pub fn api_router() -> Router<AppState> {
    use axum::routing::get;
    Router::new()
        .route_as("/api/v1/search/issues", Access::PublicRead, get(api_issues))
        .route_as("/api/v1/search/repos", Access::PublicRead, get(api_repos))
}
