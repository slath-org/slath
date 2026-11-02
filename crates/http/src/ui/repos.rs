// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Repository pages: creation, the repository home, and settings.

use askama::Template;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Redirect, Response};
use axum::{Form, Router};
use serde::Deserialize;
use slath_core::authz::{Action, RepoState, Role, Subject, Visibility};
use slath_service::repos::{NewRepoInput, RepoAccess};
use slath_service::{FieldError, ServiceError};

use super::settings::error_for;
use super::{PageCtx, render};
use crate::AppState;
use crate::auth::{MaybeSession, RequireSession, Sudo};
use crate::error::AppError;
use crate::routes::{Access, RouteExt};

/// The form and badge value of a visibility.
#[must_use]
pub const fn visibility_name(v: Visibility) -> &'static str {
    slath_db::repos::visibility_str(v)
}

/// A visibility from a form or API value.
#[must_use]
pub fn parse_visibility(s: &str) -> Option<Visibility> {
    match s {
        "public" => Some(Visibility::Public),
        "internal" => Some(Visibility::Internal),
        "private" => Some(Visibility::Private),
        _ => None,
    }
}

/// A repository in a list (profile pages, explore).
#[derive(Debug, Clone)]
pub struct RepoCard {
    pub namespace: String,
    pub name: String,
    pub description: Option<String>,
    /// `public`, `internal`, or `private`.
    pub visibility: &'static str,
    pub archived: bool,
}

impl From<&slath_db::repos::Repo> for RepoCard {
    fn from(r: &slath_db::repos::Repo) -> Self {
        Self {
            namespace: r.namespace.clone(),
            name: r.name.clone(),
            description: r.description.clone(),
            visibility: visibility_name(r.visibility),
            archived: r.state == RepoState::Archived,
        }
    }
}

/// The repositories of a namespace the subject can see.
pub async fn cards_for_namespace(
    state: &AppState,
    subject: &Subject,
    ns: &str,
) -> Result<Vec<RepoCard>, AppError> {
    let Some((ns_id, _, _)) = slath_db::orgs::resolve_namespace(&state.pool, ns)
        .await
        .map_err(|e| AppError::internal(&e))?
    else {
        return Ok(Vec::new());
    };
    Ok(state
        .services
        .list_namespace_repos(subject, ns_id)
        .await?
        .iter()
        .map(Into::into)
        .collect())
}

/// Data for the repository header shown on every repository page.
#[derive(Debug, Clone)]
pub struct RepoHeader {
    pub namespace: String,
    pub name: String,
    /// `public`, `internal`, or `private`.
    pub visibility: &'static str,
    pub archived: bool,
    pub can_admin: bool,
    /// The repository has issues.
    pub issues: bool,
    /// Which tab is active: `code`, `issues`, `settings`, ….
    pub tab: &'static str,
    /// The viewer's subscription (`watching`, `ignoring`, `default`), shown
    /// to signed-in viewers on every repository page.
    pub watch: Option<&'static str>,
    /// The import status, shown on the front page.
    pub import: Option<ImportView>,
}

/// An import as the front page shows it.
#[derive(Debug, Clone)]
pub struct ImportView {
    /// `queued`, `running`, `done`, or `failed`.
    pub state: String,
    /// Source URL and details (administrators only).
    pub url: Option<String>,
    pub error: Option<String>,
    pub branches: i32,
    pub tags: i32,
}

impl RepoHeader {
    #[must_use]
    pub fn new(a: &RepoAccess, subject: &Subject, tab: &'static str) -> Self {
        Self {
            namespace: a.repo.namespace.clone(),
            name: a.repo.name.clone(),
            visibility: visibility_name(a.repo.visibility),
            archived: a.repo.state == RepoState::Archived,
            can_admin: a.can(subject, Action::RepoSettings),
            issues: a.repo.issues_enabled,
            tab,
            watch: None,
            import: None,
        }
    }

    /// The header with the viewer's watch state.
    pub async fn load(
        state: &AppState,
        a: &RepoAccess,
        subject: &Subject,
        tab: &'static str,
    ) -> Self {
        let mut header = Self::new(a, subject, tab);
        if !matches!(subject, Subject::Anonymous) {
            use slath_service::notifications::Subscription;
            header.watch = Some(match state.services.repo_subscription(subject, a).await {
                Subscription::Watching => "watching",
                Subscription::Ignoring => "ignoring",
                Subscription::Default => "default",
            });
        }
        header
    }

    /// Adds the import status (front page only).
    pub async fn with_import(
        mut self,
        state: &AppState,
        a: &RepoAccess,
        subject: &Subject,
    ) -> Self {
        // Finished imports are reported for a week.
        let week_ago = time::OffsetDateTime::now_utc() - time::Duration::days(7);
        if let Ok(Some(i)) = state.services.import_report(subject, a).await
            && !(i.state == "done" && i.finished_at.is_some_and(|t| t < week_ago))
        {
            let admin = self.can_admin;
            self.import = Some(ImportView {
                state: i.state,
                url: admin.then_some(i.url),
                error: if admin { i.error } else { None },
                branches: i.branches.unwrap_or(0),
                tags: i.tags.unwrap_or(0),
            });
        }
        self
    }
}

#[derive(Template)]
#[template(path = "repo/new.html")]
struct NewRepoPage {
    ctx: PageCtx,
    namespaces: Vec<String>,
    selected: String,
    name: String,
    description: String,
    visibility: &'static str,
    default_branch: String,
    errors: Vec<FieldError>,
}

impl NewRepoPage {
    fn error(&self, field: &str) -> Option<&str> {
        error_for(&self.errors, field)
    }
}

async fn creatable_namespaces(
    state: &AppState,
    session: &slath_service::sessions::Session,
) -> Result<Vec<String>, AppError> {
    let mut out = vec![session.row.user_name.clone()];
    for (org, _) in slath_db::orgs::for_user(&state.pool, session.user_id())
        .await
        .map_err(|e| AppError::internal(&e))?
    {
        out.push(org);
    }
    Ok(out)
}

#[derive(Deserialize, Default)]
struct NewQuery {
    namespace: Option<String>,
}

async fn new_repo(
    State(state): State<AppState>,
    ctx: PageCtx,
    RequireSession(session): RequireSession,
    Query(q): Query<NewQuery>,
) -> Result<Response, AppError> {
    let namespaces = creatable_namespaces(&state, &session).await?;
    let selected = q
        .namespace
        .filter(|n| namespaces.iter().any(|x| x.eq_ignore_ascii_case(n)))
        .unwrap_or_else(|| session.row.user_name.clone());
    Ok(render(&NewRepoPage {
        ctx,
        namespaces,
        selected,
        name: String::new(),
        description: String::new(),
        visibility: "private",
        default_branch: "main".to_owned(),
        errors: Vec::new(),
    }))
}

#[derive(Deserialize)]
struct NewRepoForm {
    #[serde(default)]
    namespace: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    visibility: String,
    #[serde(default)]
    default_branch: String,
}

async fn create_repo(
    State(state): State<AppState>,
    ctx: PageCtx,
    RequireSession(session): RequireSession,
    Form(f): Form<NewRepoForm>,
) -> Result<Response, AppError> {
    let visibility = parse_visibility(&f.visibility).unwrap_or(Visibility::Private);
    let input = NewRepoInput {
        namespace: f.namespace.clone(),
        name: f.name.clone(),
        description: f.description.clone(),
        visibility,
        default_branch: f.default_branch.clone(),
    };
    match state.services.create_repo(&session.subject, input).await {
        Ok(a) => {
            Ok(Redirect::to(&format!("/{}/{}", a.repo.namespace, a.repo.name)).into_response())
        }
        Err(ServiceError::Invalid(errors)) => {
            let namespaces = creatable_namespaces(&state, &session).await?;
            let mut res = render(&NewRepoPage {
                ctx,
                namespaces,
                selected: f.namespace,
                name: f.name,
                description: f.description,
                visibility: visibility_name(visibility),
                default_branch: f.default_branch,
                errors,
            });
            *res.status_mut() = StatusCode::UNPROCESSABLE_ENTITY;
            Ok(res)
        }
        Err(e) => Err(e.into()),
    }
}

#[derive(Template)]
#[template(path = "repo/home.html")]
struct RepoHome {
    ctx: PageCtx,
    header: RepoHeader,
    description: Option<String>,
    https_url: String,
    ssh_url: String,
    empty: bool,
    default_branch: String,
    can_push: bool,
}

/// Clone URLs: HTTPS from `public_url`, SSH from `[git]`.
pub fn clone_urls(state: &AppState, ns: &str, name: &str) -> (String, String) {
    let cfg = &state.config;
    let https = cfg.server.public_url.join(&format!("/{ns}/{name}.git"));
    let host = if cfg.git.ssh_host.is_empty() {
        cfg.server.public_url.host()
    } else {
        cfg.git.ssh_host.as_str()
    };
    let ssh = if cfg.git.ssh_port == 22 {
        format!("{}@{host}:{ns}/{name}.git", cfg.git.ssh_user)
    } else {
        format!(
            "ssh://{}@{host}:{}/{ns}/{name}.git",
            cfg.git.ssh_user, cfg.git.ssh_port
        )
    };
    (https, ssh)
}

/// `GET /{ns}/{repo}`.
pub async fn repo_home(
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
    let empty = slath_git::storage::is_empty(&state.runner, &state.services.repo_path(a.repo.id))
        .await
        .map_err(|e| AppError::internal(&e))?;
    if !empty {
        return super::browse::repo_home_with_content(state, ctx, subject, a).await;
    }
    let (https_url, ssh_url) = clone_urls(&state, &a.repo.namespace, &a.repo.name);
    Ok(render(&RepoHome {
        header: RepoHeader::load(&state, &a, &subject, "code")
            .await
            .with_import(&state, &a, &subject)
            .await,
        description: a.repo.description.clone(),
        can_push: a.can(&subject, Action::RepoPush),
        default_branch: a.repo.default_branch.clone(),
        ctx,
        https_url,
        ssh_url,
        empty,
    }))
}

#[derive(Template)]
#[template(path = "repo/settings_general.html")]
struct GeneralSettings<'a> {
    ctx: PageCtx,
    header: RepoHeader,
    description: String,
    default_branch: String,
    name: String,
    visibility: &'static str,
    archived: bool,
    message: Option<&'a str>,
    errors: Vec<FieldError>,
}

impl GeneralSettings<'_> {
    fn error(&self, field: &str) -> Option<&str> {
        error_for(&self.errors, field)
    }
}

#[derive(Deserialize, Default)]
struct StatusQuery {
    status: Option<String>,
}

fn status_message(s: Option<&str>) -> Option<&'static str> {
    Some(match s? {
        "saved" => "Settings saved.",
        "visibility" => "Visibility changed.",
        "renamed" => "Repository renamed. The old name redirects here.",
        "archived" => "Repository archived. It's read-only now.",
        "unarchived" => "Repository unarchived.",
        "collaborator-saved" => "Collaborator saved.",
        "collaborator-removed" => "Collaborator removed.",
        "team-saved" => "Team access saved.",
        "rule-saved" => "Protection rule saved.",
        "rule-removed" => "Protection rule removed.",
        "team-removed" => "Team access removed.",
        _ => return None,
    })
}

async fn settings_access(
    state: &AppState,
    subject: &Subject,
    ns: &str,
    repo: &str,
) -> Result<RepoAccess, AppError> {
    // Settings pages exist only for administrators; others get "not found".
    // Gate on archiving, which administrators keep on archived repositories
    // (to unarchive); each change is checked again for its own action.
    match state
        .services
        .repo_access(subject, ns, repo, Action::RepoArchive)
        .await
    {
        Err(ServiceError::Forbidden) => Err(AppError::not_found()),
        other => Ok(other?),
    }
}

fn general_page<'a>(
    header: RepoHeader,
    a: &RepoAccess,
    ctx: PageCtx,
    message: Option<&'a str>,
    errors: Vec<FieldError>,
) -> GeneralSettings<'a> {
    GeneralSettings {
        header,
        description: a.repo.description.clone().unwrap_or_default(),
        default_branch: a.repo.default_branch.clone(),
        name: a.repo.name.clone(),
        visibility: visibility_name(a.repo.visibility),
        archived: a.repo.state == RepoState::Archived,
        ctx,
        message,
        errors,
    }
}

async fn settings_general(
    State(state): State<AppState>,
    ctx: PageCtx,
    session: MaybeSession,
    Path((ns, repo)): Path<(String, String)>,
    Query(q): Query<StatusQuery>,
) -> Result<Response, AppError> {
    let subject = session.subject();
    let a = settings_access(&state, &subject, &ns, &repo).await?;
    Ok(render(&general_page(
        RepoHeader::load(&state, &a, &subject, "settings").await,
        &a,
        ctx,
        status_message(q.status.as_deref()),
        Vec::new(),
    )))
}

fn repo_url(a: &RepoAccess, rest: &str) -> String {
    format!("/{}/{}{rest}", a.repo.namespace, a.repo.name)
}

async fn invalid_page(
    state: &AppState,
    a: &RepoAccess,
    subject: &Subject,
    ctx: PageCtx,
    e: ServiceError,
) -> Result<Response, AppError> {
    match e {
        ServiceError::Invalid(errors) => {
            let header = RepoHeader::load(state, a, subject, "settings").await;
            let mut res = render(&general_page(header, a, ctx, None, errors));
            *res.status_mut() = StatusCode::UNPROCESSABLE_ENTITY;
            Ok(res)
        }
        ServiceError::Forbidden => Err(AppError::not_found()),
        e => Err(e.into()),
    }
}

#[derive(Deserialize)]
struct GeneralForm {
    #[serde(default)]
    description: String,
    #[serde(default)]
    default_branch: String,
}

async fn save_general(
    State(state): State<AppState>,
    ctx: PageCtx,
    RequireSession(s): RequireSession,
    Path((ns, repo)): Path<(String, String)>,
    Form(f): Form<GeneralForm>,
) -> Result<Response, AppError> {
    let a = settings_access(&state, &s.subject, &ns, &repo).await?;
    match state
        .services
        .update_repo_settings(&s.subject, &a, &f.description, &f.default_branch)
        .await
    {
        Ok(()) => Ok(Redirect::to(&repo_url(&a, "/settings/general?status=saved")).into_response()),
        Err(e) => invalid_page(&state, &a, &s.subject, ctx, e).await,
    }
}

#[derive(Deserialize)]
struct VisibilityForm {
    #[serde(default)]
    visibility: String,
}

async fn save_visibility(
    State(state): State<AppState>,
    ctx: PageCtx,
    Sudo(s): Sudo,
    Path((ns, repo)): Path<(String, String)>,
    Form(f): Form<VisibilityForm>,
) -> Result<Response, AppError> {
    let a = settings_access(&state, &s.subject, &ns, &repo).await?;
    let v = match parse_visibility(&f.visibility) {
        Some(v) => v,
        None => {
            return invalid_page(
                &state,
                &a,
                &s.subject,
                ctx,
                ServiceError::invalid(
                    "visibility",
                    "invalid",
                    "Choose private, internal, or public.",
                ),
            )
            .await;
        }
    };
    match state
        .services
        .set_repo_visibility(&s.subject, true, &a, v)
        .await
    {
        Ok(()) => {
            Ok(Redirect::to(&repo_url(&a, "/settings/general?status=visibility")).into_response())
        }
        Err(e) => invalid_page(&state, &a, &s.subject, ctx, e).await,
    }
}

#[derive(Deserialize)]
struct RenameForm {
    #[serde(default)]
    name: String,
}

async fn rename(
    State(state): State<AppState>,
    ctx: PageCtx,
    RequireSession(s): RequireSession,
    Path((ns, repo)): Path<(String, String)>,
    Form(f): Form<RenameForm>,
) -> Result<Response, AppError> {
    let a = settings_access(&state, &s.subject, &ns, &repo).await?;
    match state.services.rename_repo(&s.subject, &a, &f.name).await {
        Ok(new) => Ok(Redirect::to(&format!(
            "/{}/{new}/settings/general?status=renamed",
            a.repo.namespace
        ))
        .into_response()),
        Err(e) => invalid_page(&state, &a, &s.subject, ctx, e).await,
    }
}

async fn archive(
    State(state): State<AppState>,
    RequireSession(s): RequireSession,
    Path((ns, repo, op)): Path<(String, String, String)>,
) -> Result<Response, AppError> {
    let a = settings_access(&state, &s.subject, &ns, &repo).await?;
    let archived = match op.as_str() {
        "archive" => true,
        "unarchive" => false,
        _ => return Err(AppError::not_found()),
    };
    state
        .services
        .set_repo_archived(&s.subject, &a, archived)
        .await?;
    let status = if archived { "archived" } else { "unarchived" };
    Ok(Redirect::to(&repo_url(&a, &format!("/settings/general?status={status}"))).into_response())
}

struct CollaboratorView {
    name: String,
    role: &'static str,
}

#[derive(Template)]
#[template(path = "repo/settings_access.html")]
struct AccessSettings<'a> {
    ctx: PageCtx,
    header: RepoHeader,
    collaborators: Vec<CollaboratorView>,
    message: Option<&'a str>,
    errors: Vec<FieldError>,
    username: String,
    /// Teams with access; `None` for repositories of users (no teams).
    teams: Option<Vec<(String, &'static str)>>,
}

impl AccessSettings<'_> {
    fn error(&self, field: &str) -> Option<&str> {
        error_for(&self.errors, field)
    }
}

const ROLES: [(&str, Role); 4] = [
    ("read", Role::Read),
    ("write", Role::Write),
    ("maintain", Role::Maintain),
    ("admin", Role::Admin),
];

async fn access_page(
    state: &AppState,
    ctx: PageCtx,
    subject: &Subject,
    a: &RepoAccess,
    message: Option<&str>,
    errors: Vec<FieldError>,
    username: String,
) -> Result<Response, AppError> {
    let collaborators = state
        .services
        .list_collaborators(subject, a)
        .await?
        .into_iter()
        .map(|c| CollaboratorView {
            name: c.name,
            role: slath_db::repos::role_str(c.role),
        })
        .collect();
    let status = if errors.is_empty() {
        StatusCode::OK
    } else {
        StatusCode::UNPROCESSABLE_ENTITY
    };
    let teams = if a.repo.namespace_kind == "org" {
        Some(
            state
                .services
                .repo_teams(subject, &a.repo.namespace, &a.repo.name)
                .await?
                .into_iter()
                .map(|(n, r)| (n, slath_db::repos::role_str(r)))
                .collect(),
        )
    } else {
        None
    };
    let mut res = render(&AccessSettings {
        ctx,
        header: RepoHeader::load(state, a, subject, "settings").await,
        collaborators,
        message,
        errors,
        username,
        teams,
    });
    *res.status_mut() = status;
    Ok(res)
}

async fn settings_access_page(
    State(state): State<AppState>,
    ctx: PageCtx,
    session: MaybeSession,
    Path((ns, repo)): Path<(String, String)>,
    Query(q): Query<StatusQuery>,
) -> Result<Response, AppError> {
    let subject = session.subject();
    let a = settings_access(&state, &subject, &ns, &repo).await?;
    access_page(
        &state,
        ctx,
        &subject,
        &a,
        status_message(q.status.as_deref()),
        Vec::new(),
        String::new(),
    )
    .await
}

#[derive(Deserialize)]
struct CollaboratorForm {
    #[serde(default)]
    username: String,
    #[serde(default)]
    role: String,
}

async fn add_collaborator(
    State(state): State<AppState>,
    ctx: PageCtx,
    RequireSession(s): RequireSession,
    Path((ns, repo)): Path<(String, String)>,
    Form(f): Form<CollaboratorForm>,
) -> Result<Response, AppError> {
    let a = settings_access(&state, &s.subject, &ns, &repo).await?;
    let role = ROLES
        .iter()
        .find(|(n, _)| *n == f.role)
        .map_or(Role::Read, |(_, r)| *r);
    match state
        .services
        .set_collaborator(&s.subject, &a, &f.username, role)
        .await
    {
        Ok(()) => Ok(
            Redirect::to(&repo_url(&a, "/settings/access?status=collaborator-saved"))
                .into_response(),
        ),
        Err(ServiceError::Invalid(errors)) => {
            access_page(&state, ctx, &s.subject, &a, None, errors, f.username).await
        }
        Err(e) => Err(e.into()),
    }
}

async fn remove_collaborator(
    State(state): State<AppState>,
    RequireSession(s): RequireSession,
    Path((ns, repo, user)): Path<(String, String, String)>,
) -> Result<Response, AppError> {
    let a = settings_access(&state, &s.subject, &ns, &repo).await?;
    state
        .services
        .remove_collaborator(&s.subject, &a, &user)
        .await?;
    Ok(Redirect::to(&repo_url(
        &a,
        "/settings/access?status=collaborator-removed",
    ))
    .into_response())
}

#[derive(Template)]
#[template(path = "repo/settings_protections.html")]
struct ProtectionsSettings<'a> {
    ctx: PageCtx,
    header: RepoHeader,
    rules: Vec<slath_db::protections::Protection>,
    message: Option<&'a str>,
    errors: Vec<FieldError>,
    pattern: String,
}

impl ProtectionsSettings<'_> {
    fn error(&self, field: &str) -> Option<&str> {
        error_for(&self.errors, field)
    }
}

async fn protections_page(
    state: &AppState,
    ctx: PageCtx,
    subject: &Subject,
    a: &RepoAccess,
    message: Option<&str>,
    errors: Vec<FieldError>,
    pattern: String,
) -> Result<Response, AppError> {
    let rules = state
        .services
        .protections(subject, &a.repo.namespace, &a.repo.name)
        .await?;
    let status = if errors.is_empty() {
        StatusCode::OK
    } else {
        StatusCode::UNPROCESSABLE_ENTITY
    };
    let mut res = render(&ProtectionsSettings {
        ctx,
        header: RepoHeader::load(state, a, subject, "settings").await,
        rules,
        message,
        errors,
        pattern,
    });
    *res.status_mut() = status;
    Ok(res)
}

async fn settings_protections_page(
    State(state): State<AppState>,
    ctx: PageCtx,
    session: MaybeSession,
    Path((ns, repo)): Path<(String, String)>,
    Query(q): Query<StatusQuery>,
) -> Result<Response, AppError> {
    let subject = session.subject();
    let a = settings_access(&state, &subject, &ns, &repo).await?;
    protections_page(
        &state,
        ctx,
        &subject,
        &a,
        status_message(q.status.as_deref()),
        Vec::new(),
        String::new(),
    )
    .await
}

#[derive(Deserialize)]
struct RuleForm {
    #[serde(default)]
    pattern: String,
    #[serde(default)]
    push_role: String,
    block_force_push: Option<String>,
    block_deletion: Option<String>,
    require_mr: Option<String>,
}

async fn save_protection(
    State(state): State<AppState>,
    ctx: PageCtx,
    RequireSession(s): RequireSession,
    Path((ns, repo)): Path<(String, String)>,
    Form(f): Form<RuleForm>,
) -> Result<Response, AppError> {
    let a = settings_access(&state, &s.subject, &ns, &repo).await?;
    match state
        .services
        .set_protection(
            &s.subject,
            &a.repo.namespace,
            &a.repo.name,
            slath_service::protections::RuleInput {
                pattern: f.pattern.clone(),
                block_force_push: f.block_force_push.is_some(),
                block_deletion: f.block_deletion.is_some(),
                require_mr: f.require_mr.is_some(),
                push_role: f.push_role,
            },
        )
        .await
    {
        Ok(()) => Ok(
            Redirect::to(&repo_url(&a, "/settings/protections?status=rule-saved")).into_response(),
        ),
        Err(ServiceError::Invalid(errors)) => {
            protections_page(&state, ctx, &s.subject, &a, None, errors, f.pattern).await
        }
        Err(e) => Err(e.into()),
    }
}

async fn delete_protection(
    State(state): State<AppState>,
    RequireSession(s): RequireSession,
    Path((ns, repo, id)): Path<(String, String, String)>,
) -> Result<Response, AppError> {
    let a = settings_access(&state, &s.subject, &ns, &repo).await?;
    let id = id.parse().map_err(|_| AppError::not_found())?;
    state
        .services
        .delete_protection(&s.subject, &a.repo.namespace, &a.repo.name, id)
        .await?;
    Ok(Redirect::to(&repo_url(&a, "/settings/protections?status=rule-removed")).into_response())
}

#[derive(Deserialize)]
struct TeamAccessForm {
    #[serde(default)]
    team: String,
    #[serde(default)]
    role: String,
}

async fn set_team(
    State(state): State<AppState>,
    ctx: PageCtx,
    RequireSession(s): RequireSession,
    Path((ns, repo)): Path<(String, String)>,
    Form(f): Form<TeamAccessForm>,
) -> Result<Response, AppError> {
    let a = settings_access(&state, &s.subject, &ns, &repo).await?;
    let role = ROLES
        .iter()
        .find(|(n, _)| *n == f.role)
        .map_or(Role::Read, |(_, r)| *r);
    match state
        .services
        .set_repo_team(
            &s.subject,
            &a.repo.namespace,
            &a.repo.name,
            &f.team,
            Some(role),
        )
        .await
    {
        Ok(()) => {
            Ok(Redirect::to(&repo_url(&a, "/settings/access?status=team-saved")).into_response())
        }
        Err(ServiceError::Invalid(errors)) => {
            access_page(&state, ctx, &s.subject, &a, None, errors, String::new()).await
        }
        Err(e) => Err(e.into()),
    }
}

async fn remove_team(
    State(state): State<AppState>,
    RequireSession(s): RequireSession,
    Path((ns, repo, team)): Path<(String, String, String)>,
) -> Result<Response, AppError> {
    let a = settings_access(&state, &s.subject, &ns, &repo).await?;
    state
        .services
        .set_repo_team(&s.subject, &a.repo.namespace, &a.repo.name, &team, None)
        .await?;
    Ok(Redirect::to(&repo_url(&a, "/settings/access?status=team-removed")).into_response())
}

#[derive(Template)]
#[template(path = "repo/settings_danger.html")]
struct DangerSettings {
    ctx: PageCtx,
    header: RepoHeader,
    full_name: String,
    error: Option<&'static str>,
}

async fn settings_danger(
    State(state): State<AppState>,
    ctx: PageCtx,
    session: MaybeSession,
    Path((ns, repo)): Path<(String, String)>,
) -> Result<Response, AppError> {
    let subject = session.subject();
    let a = settings_access(&state, &subject, &ns, &repo).await?;
    Ok(render(&DangerSettings {
        header: RepoHeader::load(&state, &a, &subject, "settings").await,
        full_name: format!("{}/{}", a.repo.namespace, a.repo.name),
        ctx,
        error: None,
    }))
}

#[derive(Deserialize)]
struct DeleteForm {
    #[serde(default)]
    confirm: String,
}

async fn delete_repo(
    State(state): State<AppState>,
    ctx: PageCtx,
    Sudo(s): Sudo,
    Path((ns, repo)): Path<(String, String)>,
    Form(f): Form<DeleteForm>,
) -> Result<Response, AppError> {
    let a = settings_access(&state, &s.subject, &ns, &repo).await?;
    let full_name = format!("{}/{}", a.repo.namespace, a.repo.name);
    if !f.confirm.trim().eq_ignore_ascii_case(&full_name) {
        let mut res = render(&DangerSettings {
            header: RepoHeader::load(&state, &a, &s.subject, "settings").await,
            full_name,
            ctx,
            error: Some("Type the repository's full name to confirm."),
        });
        *res.status_mut() = StatusCode::UNPROCESSABLE_ENTITY;
        return Ok(res);
    }
    state.services.delete_repo(&s.subject, true, &a).await?;
    Ok(Redirect::to(&format!("/{}", a.repo.namespace)).into_response())
}

pub fn router() -> Router<AppState> {
    use axum::routing::{get, post};
    Router::new()
        .route_as("/-/new", Access::Account, get(new_repo).post(create_repo))
        .route_as("/{ns}/{repo}", Access::Resource, get(repo_home))
        .route_as(
            "/{ns}/{repo}/settings",
            Access::Resource,
            get(|Path((ns, repo)): Path<(String, String)>| async move {
                Redirect::to(&format!("/{ns}/{repo}/settings/general"))
            }),
        )
        .route_as(
            "/{ns}/{repo}/settings/general",
            Access::Resource,
            get(settings_general).post(save_general),
        )
        .route_as(
            "/{ns}/{repo}/settings/visibility",
            Access::Resource,
            post(save_visibility),
        )
        .route_as(
            "/{ns}/{repo}/settings/rename",
            Access::Resource,
            post(rename),
        )
        .route_as(
            "/{ns}/{repo}/settings/state/{op}",
            Access::Resource,
            post(archive),
        )
        .route_as(
            "/{ns}/{repo}/settings/access",
            Access::Resource,
            get(settings_access_page).post(add_collaborator),
        )
        .route_as(
            "/{ns}/{repo}/settings/access/{user}/remove",
            Access::Resource,
            post(remove_collaborator),
        )
        .route_as(
            "/{ns}/{repo}/settings/protections",
            Access::Resource,
            get(settings_protections_page).post(save_protection),
        )
        .route_as(
            "/{ns}/{repo}/settings/protections/{id}/delete",
            Access::Resource,
            post(delete_protection),
        )
        .route_as(
            "/{ns}/{repo}/settings/access/teams",
            Access::Resource,
            post(set_team),
        )
        .route_as(
            "/{ns}/{repo}/settings/access/teams/{team}/remove",
            Access::Resource,
            post(remove_team),
        )
        .route_as(
            "/{ns}/{repo}/settings/danger",
            Access::Resource,
            get(settings_danger).post(delete_repo),
        )
}
