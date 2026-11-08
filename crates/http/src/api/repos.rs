// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! `/repos` and everything under `/repos/{ns}/{repo}`.

use axum::extract::{OriginalUri, Path, Query, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::{Json, Router};
use base64::Engine;
use serde::{Deserialize, Serialize};
use slath_core::authz::{Action, RepoState, Role, Subject, Visibility};
use slath_git::ObjectReader;
use slath_git::args::RepoPath;
use slath_git::browse::{self, PathObject, RefKind};
use slath_git::object::{Blob, EntryKind, ObjectId};
use slath_service::repos::{NewRepoInput, RepoAccess};
use utoipa::{IntoParams, ToSchema};

use super::support::{PageParams, page};
use super::{ApiSubject, ApiUser};
use crate::AppState;
use crate::error::{AppError, FieldError};
use crate::routes::{Access, RouteExt};

fn ts(t: time::OffsetDateTime) -> String {
    crate::ui::Time::from(t).iso
}

fn lossy(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

#[derive(Serialize, ToSchema)]
pub struct CloneUrls {
    pub https: String,
    pub ssh: String,
}

#[derive(Serialize, ToSchema)]
pub struct Repo {
    pub id: String,
    pub namespace: String,
    pub name: String,
    /// `namespace/name`.
    pub full_name: String,
    pub description: Option<String>,
    /// `public`, `internal` (signed-in users), or `private`.
    pub visibility: String,
    pub default_branch: String,
    pub archived: bool,
    pub clone_urls: CloneUrls,
    pub created_at: String,
    pub updated_at: String,
    pub pushed_at: Option<String>,
}

fn repo_json(state: &AppState, r: &slath_db::repos::Repo) -> Repo {
    let (https, ssh) = crate::ui::repos::clone_urls(state, &r.namespace, &r.name);
    Repo {
        id: r.id.to_string(),
        namespace: r.namespace.clone(),
        name: r.name.clone(),
        full_name: format!("{}/{}", r.namespace, r.name),
        description: r.description.clone(),
        visibility: slath_db::repos::visibility_str(r.visibility).to_owned(),
        default_branch: r.default_branch.clone(),
        archived: r.state == RepoState::Archived,
        clone_urls: CloneUrls { https, ssh },
        created_at: ts(r.created_at),
        updated_at: ts(r.updated_at),
        pushed_at: r.pushed_at.map(ts),
    }
}

#[derive(Serialize, ToSchema)]
pub struct RepoPage {
    pub items: Vec<Repo>,
    pub next_cursor: Option<String>,
}

/// Repositories the caller can see (public ones for anonymous callers).
#[utoipa::path(get, path = "/api/v1/repos", tag = "repos", security((), ("token" = ["repo:read"])), params(PageParams),
    responses((status = 200, body = RepoPage), (status = 401, body = super::Problem)))]
pub async fn list_repos(
    State(state): State<AppState>,
    ApiSubject(subject): ApiSubject,
    OriginalUri(uri): OriginalUri,
    Query(p): Query<PageParams>,
) -> Result<Response, AppError> {
    let (offset, limit) = (p.offset(&state)?, p.limit(state.config.limits.api_page_max));
    let repos = state
        .services
        .list_visible_repos(&subject, offset, limit + 1)
        .await?;
    let items: Vec<Repo> = repos.iter().map(|r| repo_json(&state, r)).collect();
    Ok(page(&state, &uri, items, offset, limit))
}

fn visibility(v: &str) -> Result<Visibility, AppError> {
    match v {
        "public" => Ok(Visibility::Public),
        "internal" => Ok(Visibility::Internal),
        "private" => Ok(Visibility::Private),
        _ => Err(AppError::validation(vec![FieldError {
            field: "visibility".to_owned(),
            code: "invalid".to_owned(),
            message: "Use public, internal, or private.".to_owned(),
        }])),
    }
}

#[derive(Deserialize, ToSchema)]
pub struct NewRepo {
    /// Your username or an organization you belong to.
    pub namespace: String,
    pub name: String,
    pub description: Option<String>,
    /// `public`, `internal` (signed-in users), or `private`.
    pub visibility: String,
    /// Defaults to `main`.
    pub default_branch: Option<String>,
}

/// Creates a repository (`repo:write`).
#[utoipa::path(post, path = "/api/v1/repos", tag = "repos", security(("token" = ["repo:write"])), request_body = NewRepo,
    responses((status = 201, body = Repo), (status = 401, body = super::Problem), (status = 403, body = super::Problem), (status = 422, body = super::Problem)))]
pub async fn create_repo(
    State(state): State<AppState>,
    ApiUser(subject): ApiUser,
    Json(n): Json<NewRepo>,
) -> Result<Response, AppError> {
    let a = state
        .services
        .create_repo(
            &subject,
            NewRepoInput {
                namespace: n.namespace,
                name: n.name,
                description: n.description.unwrap_or_default(),
                visibility: visibility(&n.visibility)?,
                default_branch: n.default_branch.unwrap_or_default(),
            },
        )
        .await?;
    let r = repo_json(&state, &a.repo);
    let loc = format!("/api/v1/repos/{}", r.full_name);
    Ok((StatusCode::CREATED, [(header::LOCATION, loc)], Json(r)).into_response())
}

async fn access(
    state: &AppState,
    subject: &Subject,
    ns: &str,
    repo: &str,
    action: Action,
) -> Result<RepoAccess, AppError> {
    Ok(state
        .services
        .repo_access(subject, ns, repo, action)
        .await?)
}

/// A repository.
#[utoipa::path(get, path = "/api/v1/repos/{ns}/{repo}", tag = "repos", security((), ("token" = ["repo:read"])), params(("ns" = String, Path), ("repo" = String, Path)),
    responses((status = 200, body = Repo), (status = 301, description = "Renamed; follow Location"), (status = 404, body = super::Problem)))]
pub async fn get_repo(
    State(state): State<AppState>,
    ApiSubject(subject): ApiSubject,
    Path((ns, repo)): Path<(String, String)>,
) -> Result<Json<Repo>, AppError> {
    let a = access(&state, &subject, &ns, &repo, Action::RepoRead).await?;
    Ok(Json(repo_json(&state, &a.repo)))
}

#[derive(Deserialize, ToSchema)]
pub struct UpdateRepo {
    pub description: Option<String>,
    pub default_branch: Option<String>,
    /// `public`, `internal` (signed-in users), or `private`.
    pub visibility: Option<String>,
    /// A new name; the old one keeps redirecting readers.
    pub name: Option<String>,
    pub archived: Option<bool>,
}

/// Changes a repository's settings (admins, `repo:write`).
#[utoipa::path(patch, path = "/api/v1/repos/{ns}/{repo}", tag = "repos", security(("token" = ["repo:write"])), params(("ns" = String, Path), ("repo" = String, Path)), request_body = UpdateRepo,
    responses((status = 200, body = Repo), (status = 401, body = super::Problem), (status = 403, body = super::Problem), (status = 404, body = super::Problem), (status = 422, body = super::Problem)))]
pub async fn update_repo(
    State(state): State<AppState>,
    ApiUser(subject): ApiUser,
    Path((ns, repo)): Path<(String, String)>,
    Json(u): Json<UpdateRepo>,
) -> Result<Json<Repo>, AppError> {
    let mut a = access(&state, &subject, &ns, &repo, Action::RepoRead).await?;
    let svc = &state.services;
    // Unarchive before and archive after the other changes: an archived
    // repository refuses every other change.
    let was_archived = a.repo.state == RepoState::Archived;
    if u.archived == Some(false) && was_archived {
        svc.set_repo_archived(&subject, &a, false).await?;
        a = svc
            .repo_access_by_id(&subject, a.repo.id, Action::RepoRead)
            .await?;
    }
    if u.description.is_some() || u.default_branch.is_some() {
        let d = u
            .description
            .or_else(|| a.repo.description.clone())
            .unwrap_or_default();
        let b = u
            .default_branch
            .unwrap_or_else(|| a.repo.default_branch.clone());
        svc.update_repo_settings(&subject, &a, &d, &b).await?;
    }
    if let Some(v) = &u.visibility {
        svc.set_repo_visibility(&subject, false, &a, visibility(v)?)
            .await?;
    }
    if let Some(n) = &u.name {
        svc.rename_repo(&subject, &a, n).await?;
    }
    if u.archived == Some(true) && !was_archived {
        svc.set_repo_archived(&subject, &a, true).await?;
    }
    let a = svc
        .repo_access_by_id(&subject, a.repo.id, Action::RepoRead)
        .await?;
    Ok(Json(repo_json(&state, &a.repo)))
}

/// Deletes a repository (admins, `repo:write`). It disappears at once and
/// can be restored by a site administrator for a few days.
#[utoipa::path(delete, path = "/api/v1/repos/{ns}/{repo}", tag = "repos", security(("token" = ["repo:write"])), params(("ns" = String, Path), ("repo" = String, Path)),
    responses((status = 204), (status = 401, body = super::Problem), (status = 403, body = super::Problem), (status = 404, body = super::Problem)))]
pub async fn delete_repo(
    State(state): State<AppState>,
    ApiUser(subject): ApiUser,
    Path((ns, repo)): Path<(String, String)>,
) -> Result<StatusCode, AppError> {
    let a = access(&state, &subject, &ns, &repo, Action::RepoRead).await?;
    state.services.delete_repo(&subject, false, &a).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Serialize, ToSchema)]
pub struct GitRef {
    pub name: String,
    pub commit: String,
}

#[derive(Serialize, ToSchema)]
pub struct GitRefPage {
    pub items: Vec<GitRef>,
    pub next_cursor: Option<String>,
}

async fn refs(
    state: &AppState,
    subject: &Subject,
    ns: &str,
    repo: &str,
    kind: RefKind,
    uri: &axum::http::Uri,
    p: &PageParams,
) -> Result<Response, AppError> {
    let a = access(state, subject, ns, repo, Action::RepoRead).await?;
    let (offset, limit) = (p.offset(state)?, p.limit(state.config.limits.api_page_max));
    let list = browse::list_refs(
        &state.runner,
        &state.services.repo_path(a.repo.id),
        kind,
        5000,
    )
    .await
    .map_err(|e| AppError::internal(&e))?;
    let items: Vec<GitRef> = list
        .into_iter()
        .skip(offset)
        .take(limit + 1)
        .map(|r| GitRef {
            name: lossy(&r.name),
            commit: r.commit.as_str().to_owned(),
        })
        .collect();
    Ok(page(state, uri, items, offset, limit))
}

/// Branches, newest first.
#[utoipa::path(get, path = "/api/v1/repos/{ns}/{repo}/branches", tag = "repos", security((), ("token" = ["repo:read"])), params(("ns" = String, Path), ("repo" = String, Path), PageParams),
    responses((status = 200, body = GitRefPage), (status = 404, body = super::Problem)))]
pub async fn list_branches(
    State(state): State<AppState>,
    ApiSubject(s): ApiSubject,
    Path((ns, repo)): Path<(String, String)>,
    OriginalUri(uri): OriginalUri,
    Query(p): Query<PageParams>,
) -> Result<Response, AppError> {
    refs(&state, &s, &ns, &repo, RefKind::Branch, &uri, &p).await
}

/// Tags, newest first.
#[utoipa::path(get, path = "/api/v1/repos/{ns}/{repo}/tags", tag = "repos", security((), ("token" = ["repo:read"])), params(("ns" = String, Path), ("repo" = String, Path), PageParams),
    responses((status = 200, body = GitRefPage), (status = 404, body = super::Problem)))]
pub async fn list_tags(
    State(state): State<AppState>,
    ApiSubject(s): ApiSubject,
    Path((ns, repo)): Path<(String, String)>,
    OriginalUri(uri): OriginalUri,
    Query(p): Query<PageParams>,
) -> Result<Response, AppError> {
    refs(&state, &s, &ns, &repo, RefKind::Tag, &uri, &p).await
}

#[derive(Serialize, ToSchema)]
pub struct Commit {
    pub oid: String,
    pub parents: Vec<String>,
    pub author_name: String,
    pub author_email: String,
    pub authored_at: String,
    pub subject: String,
}

#[derive(Serialize, ToSchema)]
pub struct CommitPage {
    pub items: Vec<Commit>,
    pub next_cursor: Option<String>,
}

#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct CommitQuery {
    /// Branch, tag, or commit ID (default: the default branch).
    #[serde(rename = "ref")]
    pub reference: Option<String>,
    /// Only commits touching this path.
    pub path: Option<String>,
}

fn unix(t: i64) -> String {
    ts(time::OffsetDateTime::from_unix_timestamp(t).unwrap_or(time::OffsetDateTime::UNIX_EPOCH))
}

async fn resolve(
    state: &AppState,
    a: &RepoAccess,
    reference: &str,
) -> Result<browse::RefPath, AppError> {
    let r = browse::resolve_ref_path(
        &state.runner,
        &state.services.repo_path(a.repo.id),
        reference.as_bytes(),
    )
    .await
    .map_err(|e| AppError::internal(&e))?
    .ok_or_else(AppError::not_found)?;
    if !r.path.is_root() {
        return Err(AppError::not_found());
    }
    Ok(r)
}

fn repo_path(path: Option<&str>) -> Result<RepoPath, AppError> {
    RepoPath::parse(path.unwrap_or("").trim_matches('/').as_bytes().to_vec())
        .map_err(|_| AppError::not_found())
}

/// History of a ref, optionally limited to a path.
#[utoipa::path(get, path = "/api/v1/repos/{ns}/{repo}/commits", tag = "repos", security((), ("token" = ["repo:read"])), params(("ns" = String, Path), ("repo" = String, Path), CommitQuery, PageParams),
    responses((status = 200, body = CommitPage), (status = 404, body = super::Problem)))]
pub async fn list_commits(
    State(state): State<AppState>,
    ApiSubject(s): ApiSubject,
    Path((ns, repo)): Path<(String, String)>,
    OriginalUri(uri): OriginalUri,
    Query(q): Query<CommitQuery>,
    Query(p): Query<PageParams>,
) -> Result<Response, AppError> {
    let a = access(&state, &s, &ns, &repo, Action::RepoRead).await?;
    let r = resolve(
        &state,
        &a,
        q.reference.as_deref().unwrap_or(&a.repo.default_branch),
    )
    .await?;
    let path = repo_path(q.path.as_deref())?;
    let (offset, limit) = (p.offset(&state)?, p.limit(state.config.limits.api_page_max));
    let log = browse::log(
        &state.runner,
        &state.services.repo_path(a.repo.id),
        &r.commit,
        &path,
        offset,
        limit + 1,
    )
    .await
    .map_err(|e| AppError::internal(&e))?;
    let items: Vec<Commit> = log
        .into_iter()
        .map(|e| Commit {
            oid: e.oid.as_str().to_owned(),
            parents: e.parents.iter().map(|p| p.as_str().to_owned()).collect(),
            author_name: e.author_name,
            author_email: e.author_email,
            authored_at: unix(e.author_time),
            subject: e.subject,
        })
        .collect();
    Ok(page(&state, &uri, items, offset, limit))
}

#[derive(Serialize, ToSchema)]
pub struct ChangedFile {
    pub path: String,
    pub old_path: Option<String>,
    /// `added`, `deleted`, `modified`, `renamed`, or `copied`.
    pub status: String,
    pub additions: u32,
    pub deletions: u32,
    pub binary: bool,
}

#[derive(Serialize, ToSchema)]
pub struct CommitDetail {
    pub oid: String,
    pub parents: Vec<String>,
    pub author_name: String,
    pub authored_at: String,
    pub committer_name: String,
    pub message: String,
    pub files: Vec<ChangedFile>,
    /// The change list was cut off at the instance's diff limits.
    pub truncated: bool,
}

/// A commit with the files it changes (compared with its first parent).
#[utoipa::path(get, path = "/api/v1/repos/{ns}/{repo}/commits/{oid}", tag = "repos", security((), ("token" = ["repo:read"])), params(("ns" = String, Path), ("repo" = String, Path), ("oid" = String, Path)),
    responses((status = 200, body = CommitDetail), (status = 404, body = super::Problem)))]
pub async fn get_commit(
    State(state): State<AppState>,
    ApiSubject(s): ApiSubject,
    Path((ns, repo, oid)): Path<(String, String, String)>,
) -> Result<Json<CommitDetail>, AppError> {
    let a = access(&state, &s, &ns, &repo, Action::RepoRead).await?;
    let git_dir = state.services.repo_path(a.repo.id);
    let oid = ObjectId::parse(oid.as_bytes()).map_err(|_| AppError::not_found())?;
    let oid = browse::resolve_commit(&state.runner, &git_dir, oid.as_str().as_bytes())
        .await
        .map_err(|e| AppError::internal(&e))?
        .ok_or_else(AppError::not_found)?;
    let mut reader = ObjectReader::open(&state.runner, &git_dir)
        .await
        .map_err(|e| AppError::internal(&e))?;
    let c = reader
        .read_raw_commit(&oid)
        .await
        .map_err(|e| AppError::internal(&e))?;
    let parents: Vec<ObjectId> = c
        .parents()
        .filter_map(|p| ObjectId::parse(p).ok())
        .collect();
    let (out, too_big) = browse::commit_diff(
        &state.runner,
        &git_dir,
        parents.first(),
        &oid,
        64 * 1024 * 1024,
    )
    .await
    .map_err(|e| AppError::internal(&e))?;
    let limits = &state.config.limits;
    let diff = slath_git::diff::parse(&out, limits.diff_max_files, limits.diff_max_lines);
    let who = |h: &[u8]| -> (String, i64) {
        let s = String::from_utf8_lossy(h);
        let name = s.split(" <").next().unwrap_or("").to_owned();
        let t = s
            .rsplit('>')
            .next()
            .and_then(|r| r.split_whitespace().next())
            .and_then(|t| t.parse().ok())
            .unwrap_or(0);
        (name, t)
    };
    let (author, at) = who(c.header(b"author").unwrap_or_default());
    let (committer, _) = who(c.header(b"committer").unwrap_or_default());
    Ok(Json(CommitDetail {
        oid: oid.as_str().to_owned(),
        parents: parents.iter().map(|p| p.as_str().to_owned()).collect(),
        author_name: author,
        authored_at: unix(at),
        committer_name: committer,
        message: lossy(&c.message),
        files: diff
            .files
            .iter()
            .map(|f| ChangedFile {
                path: lossy(f.path()),
                old_path: matches!(
                    f.status,
                    slath_git::diff::Status::Renamed | slath_git::diff::Status::Copied
                )
                .then(|| lossy(&f.old_path)),
                status: format!("{:?}", f.status).to_ascii_lowercase(),
                additions: f.additions,
                deletions: f.deletions,
                binary: f.binary,
            })
            .collect(),
        truncated: diff.truncated || too_big,
    }))
}

#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct PathQuery {
    /// Path inside the repository (default: the root).
    pub path: Option<String>,
}

#[derive(Serialize, ToSchema)]
pub struct TreeEntry {
    pub name: String,
    pub path: String,
    /// `dir`, `file`, `executable`, `symlink`, or `submodule`.
    #[serde(rename = "type")]
    pub kind: String,
    pub oid: String,
}

#[derive(Serialize, ToSchema)]
pub struct Tree {
    pub commit: String,
    pub path: String,
    pub entries: Vec<TreeEntry>,
}

/// The entries of a directory at a ref.
#[utoipa::path(get, path = "/api/v1/repos/{ns}/{repo}/tree/{ref}", tag = "repos", security((), ("token" = ["repo:read"])), params(("ns" = String, Path), ("repo" = String, Path), ("ref" = String, Path, description = "Branch, tag, or commit ID (percent-encode slashes)"), PathQuery),
    responses((status = 200, body = Tree), (status = 404, body = super::Problem)))]
pub async fn get_tree(
    State(state): State<AppState>,
    ApiSubject(s): ApiSubject,
    Path((ns, repo, reference)): Path<(String, String, String)>,
    Query(q): Query<PathQuery>,
) -> Result<Json<Tree>, AppError> {
    let a = access(&state, &s, &ns, &repo, Action::RepoRead).await?;
    let r = resolve(&state, &a, &reference).await?;
    let path = repo_path(q.path.as_deref())?;
    let mut reader = ObjectReader::open(&state.runner, &state.services.repo_path(a.repo.id))
        .await
        .map_err(|e| AppError::internal(&e))?;
    let Some(PathObject::Tree(entries)) = browse::lookup_path(&mut reader, &r.commit, &path)
        .await
        .map_err(|e| AppError::internal(&e))?
    else {
        return Err(AppError::not_found());
    };
    let base = lossy(path.as_bytes());
    Ok(Json(Tree {
        commit: r.commit.as_str().to_owned(),
        entries: entries
            .iter()
            .map(|e| TreeEntry {
                name: lossy(&e.name),
                path: if base.is_empty() {
                    lossy(&e.name)
                } else {
                    format!("{base}/{}", lossy(&e.name))
                },
                kind: match e.kind() {
                    EntryKind::Tree => "dir",
                    EntryKind::File => "file",
                    EntryKind::Executable => "executable",
                    EntryKind::Symlink => "symlink",
                    EntryKind::Submodule => "submodule",
                }
                .to_owned(),
                oid: e.oid.as_str().to_owned(),
            })
            .collect(),
        path: base,
    }))
}

#[derive(Serialize, ToSchema)]
pub struct BlobContent {
    pub commit: String,
    pub path: String,
    pub oid: String,
    pub size: u64,
    /// `utf-8` or `base64`.
    pub encoding: String,
    pub content: String,
}

/// A file's content at a ref (up to the instance's render limit).
#[utoipa::path(get, path = "/api/v1/repos/{ns}/{repo}/blob/{ref}", tag = "repos", security((), ("token" = ["repo:read"])), params(("ns" = String, Path), ("repo" = String, Path), ("ref" = String, Path), PathQuery),
    responses((status = 200, body = BlobContent), (status = 404, body = super::Problem), (status = 422, body = super::Problem)))]
pub async fn get_blob(
    State(state): State<AppState>,
    ApiSubject(s): ApiSubject,
    Path((ns, repo, reference)): Path<(String, String, String)>,
    Query(q): Query<PathQuery>,
) -> Result<Json<BlobContent>, AppError> {
    let a = access(&state, &s, &ns, &repo, Action::RepoRead).await?;
    let r = resolve(&state, &a, &reference).await?;
    let path = repo_path(q.path.as_deref())?;
    let mut reader = ObjectReader::open(&state.runner, &state.services.repo_path(a.repo.id))
        .await
        .map_err(|e| AppError::internal(&e))?;
    let Some(PathObject::Blob { entry }) = browse::lookup_path(&mut reader, &r.commit, &path)
        .await
        .map_err(|e| AppError::internal(&e))?
    else {
        return Err(AppError::not_found());
    };
    if entry.kind() == EntryKind::Submodule {
        return Err(AppError::not_found());
    }
    let limit = state.config.limits.render_max_file_bytes.as_usize();
    let data = match reader
        .read_blob(&entry.oid, limit)
        .await
        .map_err(|e| AppError::internal(&e))?
    {
        Blob::Data(d) => d,
        Blob::TooLarge { .. } => {
            return Err(AppError::validation(vec![FieldError {
                field: "path".to_owned(),
                code: "too_large".to_owned(),
                message: "The file is too large; use the raw endpoint of the web UI.".to_owned(),
            }]));
        }
    };
    let (encoding, content) = match std::str::from_utf8(&data) {
        Ok(t) if !slath_markup::is_binary(&data) => ("utf-8", t.to_owned()),
        _ => (
            "base64",
            base64::engine::general_purpose::STANDARD.encode(&data),
        ),
    };
    Ok(Json(BlobContent {
        commit: r.commit.as_str().to_owned(),
        path: lossy(path.as_bytes()),
        oid: entry.oid.as_str().to_owned(),
        size: data.len() as u64,
        encoding: encoding.to_owned(),
        content,
    }))
}

#[derive(Serialize, ToSchema)]
pub struct Collaborator {
    pub username: String,
    /// `read`, `write`, `maintain`, or `admin`.
    pub role: String,
}

#[derive(Serialize, ToSchema)]
pub struct CollaboratorPage {
    pub items: Vec<Collaborator>,
    pub next_cursor: Option<String>,
}

/// A repository's collaborators (admins).
#[utoipa::path(get, path = "/api/v1/repos/{ns}/{repo}/collaborators", tag = "repos", security(("token" = ["repo:write"])), params(("ns" = String, Path), ("repo" = String, Path), PageParams),
    responses((status = 200, body = CollaboratorPage), (status = 401, body = super::Problem), (status = 403, body = super::Problem), (status = 404, body = super::Problem)))]
pub async fn list_collaborators(
    State(state): State<AppState>,
    ApiUser(s): ApiUser,
    Path((ns, repo)): Path<(String, String)>,
    OriginalUri(uri): OriginalUri,
    Query(p): Query<PageParams>,
) -> Result<Response, AppError> {
    let a = access(&state, &s, &ns, &repo, Action::RepoRead).await?;
    let (offset, limit) = (p.offset(&state)?, p.limit(state.config.limits.api_page_max));
    let items: Vec<Collaborator> = state
        .services
        .list_collaborators(&s, &a)
        .await?
        .into_iter()
        .skip(offset)
        .take(limit + 1)
        .map(|c| Collaborator {
            username: c.name,
            role: slath_db::repos::role_str(c.role).to_owned(),
        })
        .collect();
    Ok(page(&state, &uri, items, offset, limit))
}

#[derive(Deserialize, ToSchema)]
pub struct PutCollaborator {
    /// `read`, `write`, `maintain`, or `admin`.
    pub role: String,
}

/// Adds a collaborator or changes their role (admins).
#[utoipa::path(put, path = "/api/v1/repos/{ns}/{repo}/collaborators/{user}", tag = "repos", security(("token" = ["repo:write"])), params(("ns" = String, Path), ("repo" = String, Path), ("user" = String, Path)), request_body = PutCollaborator,
    responses((status = 200, body = Collaborator), (status = 401, body = super::Problem), (status = 403, body = super::Problem), (status = 404, body = super::Problem), (status = 422, body = super::Problem)))]
pub async fn put_collaborator(
    State(state): State<AppState>,
    ApiUser(s): ApiUser,
    Path((ns, repo, user)): Path<(String, String, String)>,
    Json(c): Json<PutCollaborator>,
) -> Result<Json<Collaborator>, AppError> {
    let a = access(&state, &s, &ns, &repo, Action::RepoRead).await?;
    let role: Role = c.role.parse().map_err(|_| {
        AppError::validation(vec![FieldError {
            field: "role".to_owned(),
            code: "invalid".to_owned(),
            message: "Use read, write, maintain, or admin.".to_owned(),
        }])
    })?;
    state.services.set_collaborator(&s, &a, &user, role).await?;
    Ok(Json(Collaborator {
        username: user,
        role: c.role,
    }))
}

/// Removes a collaborator (admins).
#[utoipa::path(delete, path = "/api/v1/repos/{ns}/{repo}/collaborators/{user}", tag = "repos", security(("token" = ["repo:write"])), params(("ns" = String, Path), ("repo" = String, Path), ("user" = String, Path)),
    responses((status = 204), (status = 401, body = super::Problem), (status = 403, body = super::Problem), (status = 404, body = super::Problem)))]
pub async fn delete_collaborator(
    State(state): State<AppState>,
    ApiUser(s): ApiUser,
    Path((ns, repo, user)): Path<(String, String, String)>,
) -> Result<StatusCode, AppError> {
    let a = access(&state, &s, &ns, &repo, Action::RepoRead).await?;
    state.services.remove_collaborator(&s, &a, &user).await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Serialize, ToSchema)]
pub struct ProtectionRule {
    pub id: String,
    /// Glob on the short branch name (`*` doesn't cross `/`).
    pub pattern: String,
    pub block_force_push: bool,
    pub block_deletion: bool,
    pub require_mr: bool,
    /// `write`, `maintain`, `admin`, or `nobody`.
    pub push_role: String,
    /// Approvals by people with write access, other than the author.
    pub required_approvals: i32,
    /// New changes dismiss approvals (a rebase with identical patches doesn't).
    pub dismiss_stale_approvals: bool,
    pub require_resolved_threads: bool,
    /// The target must be an ancestor of the source.
    pub require_up_to_date: bool,
    pub bots_count_as_approvers: bool,
}

impl From<slath_db::protections::Protection> for ProtectionRule {
    fn from(p: slath_db::protections::Protection) -> Self {
        Self {
            id: p.id.to_string(),
            pattern: p.pattern,
            block_force_push: p.block_force_push,
            block_deletion: p.block_deletion,
            require_mr: p.require_mr,
            push_role: p.push_role,
            required_approvals: p.required_approvals,
            dismiss_stale_approvals: p.dismiss_stale_approvals,
            require_resolved_threads: p.require_resolved_threads,
            require_up_to_date: p.require_up_to_date,
            bots_count_as_approvers: p.bots_count_as_approvers,
        }
    }
}

/// A repository's branch protection rules (admins).
#[utoipa::path(get, path = "/api/v1/repos/{ns}/{repo}/protections", tag = "repos", security(("token" = ["repo:write"])), params(("ns" = String, Path), ("repo" = String, Path)),
    responses((status = 200, body = Vec<ProtectionRule>), (status = 401, body = super::Problem), (status = 403, body = super::Problem), (status = 404, body = super::Problem)))]
pub async fn list_protections(
    State(state): State<AppState>,
    ApiUser(s): ApiUser,
    Path((ns, repo)): Path<(String, String)>,
) -> Result<Json<Vec<ProtectionRule>>, AppError> {
    let rules = state.services.protections(&s, &ns, &repo).await?;
    Ok(Json(rules.into_iter().map(Into::into).collect()))
}

/// Every field is optional in the JSON so that authorization runs before
/// validation (a missing pattern is a 422 only for those allowed to ask).
#[derive(Deserialize, ToSchema)]
pub struct PostProtection {
    #[serde(default)]
    pub pattern: String,
    #[serde(default = "yes")]
    pub block_force_push: bool,
    #[serde(default = "yes")]
    pub block_deletion: bool,
    #[serde(default)]
    pub require_mr: bool,
    /// `write` (default), `maintain`, `admin`, or `nobody`.
    #[serde(default = "write")]
    pub push_role: String,
    /// Default 1.
    #[serde(default = "one")]
    pub required_approvals: i32,
    #[serde(default = "yes")]
    pub dismiss_stale_approvals: bool,
    #[serde(default)]
    pub require_resolved_threads: bool,
    #[serde(default)]
    pub require_up_to_date: bool,
    #[serde(default)]
    pub bots_count_as_approvers: bool,
}

const fn one() -> i32 {
    1
}

const fn yes() -> bool {
    true
}

fn write() -> String {
    "write".to_owned()
}

/// Creates the rule for a pattern, or replaces it (admins).
#[utoipa::path(post, path = "/api/v1/repos/{ns}/{repo}/protections", tag = "repos", security(("token" = ["repo:write"])), params(("ns" = String, Path), ("repo" = String, Path)), request_body = PostProtection,
    responses((status = 200, body = ProtectionRule), (status = 401, body = super::Problem), (status = 403, body = super::Problem), (status = 404, body = super::Problem), (status = 422, body = super::Problem)))]
pub async fn post_protection(
    State(state): State<AppState>,
    ApiUser(s): ApiUser,
    Path((ns, repo)): Path<(String, String)>,
    Json(p): Json<PostProtection>,
) -> Result<Json<ProtectionRule>, AppError> {
    let pattern = p.pattern.trim().to_owned();
    state
        .services
        .set_protection(
            &s,
            &ns,
            &repo,
            slath_service::protections::RuleInput {
                pattern: pattern.clone(),
                block_force_push: p.block_force_push,
                block_deletion: p.block_deletion,
                require_mr: p.require_mr,
                push_role: p.push_role,
                required_approvals: p.required_approvals,
                dismiss_stale_approvals: p.dismiss_stale_approvals,
                require_resolved_threads: p.require_resolved_threads,
                require_up_to_date: p.require_up_to_date,
                bots_count_as_approvers: p.bots_count_as_approvers,
            },
        )
        .await?;
    let rule = state
        .services
        .protections(&s, &ns, &repo)
        .await?
        .into_iter()
        .find(|r| r.pattern == pattern)
        .ok_or_else(AppError::not_found)?;
    Ok(Json(rule.into()))
}

/// Removes a rule (admins).
#[utoipa::path(delete, path = "/api/v1/repos/{ns}/{repo}/protections/{id}", tag = "repos", security(("token" = ["repo:write"])), params(("ns" = String, Path), ("repo" = String, Path), ("id" = String, Path)),
    responses((status = 204), (status = 401, body = super::Problem), (status = 403, body = super::Problem), (status = 404, body = super::Problem)))]
pub async fn delete_protection(
    State(state): State<AppState>,
    ApiUser(s): ApiUser,
    Path((ns, repo, id)): Path<(String, String, String)>,
) -> Result<StatusCode, AppError> {
    let id = id.parse().map_err(|_| AppError::not_found())?;
    state.services.delete_protection(&s, &ns, &repo, id).await?;
    Ok(StatusCode::NO_CONTENT)
}

pub fn router() -> Router<AppState> {
    use axum::routing::get;
    Router::new()
        .route_as(
            "/api/v1/repos",
            Access::PublicRead,
            get(list_repos).post(create_repo),
        )
        .route_as(
            "/api/v1/repos/{ns}/{repo}",
            Access::Resource,
            get(get_repo).patch(update_repo).delete(delete_repo),
        )
        .route_as(
            "/api/v1/repos/{ns}/{repo}/branches",
            Access::Resource,
            get(list_branches),
        )
        .route_as(
            "/api/v1/repos/{ns}/{repo}/tags",
            Access::Resource,
            get(list_tags),
        )
        .route_as(
            "/api/v1/repos/{ns}/{repo}/commits",
            Access::Resource,
            get(list_commits),
        )
        .route_as(
            "/api/v1/repos/{ns}/{repo}/commits/{oid}",
            Access::Resource,
            get(get_commit),
        )
        .route_as(
            "/api/v1/repos/{ns}/{repo}/tree/{ref}",
            Access::Resource,
            get(get_tree),
        )
        .route_as(
            "/api/v1/repos/{ns}/{repo}/blob/{ref}",
            Access::Resource,
            get(get_blob),
        )
        .route_as(
            "/api/v1/repos/{ns}/{repo}/collaborators",
            Access::Resource,
            get(list_collaborators),
        )
        .route_as(
            "/api/v1/repos/{ns}/{repo}/protections",
            Access::Resource,
            get(list_protections).post(post_protection),
        )
        .route_as(
            "/api/v1/repos/{ns}/{repo}/protections/{id}",
            Access::Resource,
            axum::routing::delete(delete_protection),
        )
        .route_as(
            "/api/v1/repos/{ns}/{repo}/collaborators/{user}",
            Access::Resource,
            axum::routing::put(put_collaborator).delete(delete_collaborator),
        )
}
