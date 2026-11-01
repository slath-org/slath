// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Repositories, redirects, and collaborators.

use slath_core::authz::{RepoState, Role, Visibility};
use slath_core::ids::{NamespaceId, RepoId, UserId};
use sqlx::PgExecutor;
use time::OffsetDateTime;

use crate::DbError;

#[derive(Debug, Clone)]
pub struct Repo {
    pub id: RepoId,
    pub namespace_id: NamespaceId,
    pub namespace: String,
    /// `user` or `org`.
    pub namespace_kind: String,
    pub name: String,
    pub description: Option<String>,
    pub visibility: Visibility,
    pub default_branch: String,
    pub state: RepoState,
    pub allow_forking: bool,
    pub size_bytes: i64,
    pub pushed_at: Option<OffsetDateTime>,
    pub deleted_at: Option<OffsetDateTime>,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}

pub(crate) fn visibility(s: &str) -> Visibility {
    if s == "public" {
        Visibility::Public
    } else {
        Visibility::Private
    }
}

pub(crate) fn repo_state(s: &str) -> RepoState {
    match s {
        "active" => RepoState::Active,
        "archived" => RepoState::Archived,
        _ => RepoState::Deleting,
    }
}

#[must_use]
pub const fn visibility_str(v: Visibility) -> &'static str {
    match v {
        Visibility::Public => "public",
        Visibility::Private => "private",
    }
}

#[must_use]
pub const fn role_str(r: Role) -> &'static str {
    match r {
        Role::Read => "read",
        Role::Write => "write",
        Role::Maintain => "maintain",
        Role::Admin => "admin",
    }
}

macro_rules! to_repo {
    ($r:expr) => {
        Repo {
            id: RepoId::from_uuid($r.id),
            namespace_id: NamespaceId::from_uuid($r.namespace_id),
            namespace: $r.namespace,
            namespace_kind: $r.namespace_kind,
            name: $r.name,
            description: $r.description,
            visibility: visibility(&$r.visibility),
            default_branch: $r.default_branch,
            state: repo_state(&$r.state),
            allow_forking: $r.allow_forking,
            size_bytes: $r.size_bytes,
            pushed_at: $r.pushed_at,
            deleted_at: $r.deleted_at,
            created_at: $r.created_at,
            updated_at: $r.updated_at,
        }
    };
}

pub async fn get(exec: impl PgExecutor<'_>, id: RepoId) -> Result<Option<Repo>, DbError> {
    let r = sqlx::query!(r#"SELECT r.id, r.namespace_id, n.name::text AS "namespace!", n.kind AS namespace_kind, r.name::text AS "name!",
                  r.description, r.visibility, r.default_branch, r.state, r.allow_forking, r.size_bytes,
                  r.pushed_at, r.deleted_at, r.created_at, r.updated_at
           FROM repositories r JOIN namespaces n ON n.id = r.namespace_id WHERE r.id = $1"#, id.as_uuid())
        .fetch_optional(exec)
        .await?;
    Ok(r.map(|r| to_repo!(r)))
}

/// By namespace and repository name (case-insensitive), in any state.
pub async fn find(
    exec: impl PgExecutor<'_>,
    namespace: &str,
    name: &str,
) -> Result<Option<Repo>, DbError> {
    let r = sqlx::query!(
        r#"SELECT r.id, r.namespace_id, n.name::text AS "namespace!", n.kind AS namespace_kind, r.name::text AS "name!",
                  r.description, r.visibility, r.default_branch, r.state, r.allow_forking, r.size_bytes,
                  r.pushed_at, r.deleted_at, r.created_at, r.updated_at
           FROM repositories r JOIN namespaces n ON n.id = r.namespace_id WHERE n.name = $1::citext AND r.name = $2::citext"#,
        namespace,
        name
    )
    .fetch_optional(exec)
    .await?;
    Ok(r.map(|r| to_repo!(r)))
}

/// The repository an old name redirects to, if any.
pub async fn find_redirect(
    exec: impl PgExecutor<'_>,
    namespace: &str,
    name: &str,
) -> Result<Option<RepoId>, DbError> {
    let r = sqlx::query_scalar!(
        "SELECT d.repo_id FROM repo_redirects d JOIN namespaces n ON n.id = d.namespace_id
         WHERE n.name = $1::citext AND d.name = $2::citext",
        namespace,
        name
    )
    .fetch_optional(exec)
    .await?;
    Ok(r.map(RepoId::from_uuid))
}

pub struct NewRepo<'a> {
    pub id: RepoId,
    pub namespace_id: NamespaceId,
    pub name: &'a str,
    pub description: Option<&'a str>,
    pub visibility: Visibility,
    pub default_branch: &'a str,
}

pub async fn insert(exec: impl PgExecutor<'_>, r: &NewRepo<'_>) -> Result<(), DbError> {
    sqlx::query!(
        "INSERT INTO repositories (id, namespace_id, name, description, visibility, default_branch, fork_network_id)
         VALUES ($1, $2, $3, $4, $5, $6, $1)",
        r.id.as_uuid(),
        r.namespace_id.as_uuid(),
        r.name,
        r.description,
        visibility_str(r.visibility),
        r.default_branch,
    )
    .execute(exec)
    .await
    .map_err(|e| DbError::from_unique(e, "name"))?;
    Ok(())
}

/// The facts that make up a user's effective role on a repository.
#[derive(Debug, Clone, Default)]
pub struct RoleFacts {
    pub collaborator: Option<Role>,
    /// The repository lives in the user's own namespace.
    pub namespace_owner: bool,
    /// The user's role in the owning organization (`owner`/`member`).
    pub org_role: Option<String>,
    /// The organization's role for members (`none`/`read`).
    pub org_default_member_role: Option<String>,
}

pub async fn role_facts(
    exec: impl PgExecutor<'_>,
    repo: RepoId,
    user: UserId,
) -> Result<RoleFacts, DbError> {
    let r = sqlx::query!(
        r#"SELECT
             (SELECT c.role FROM repo_collaborators c WHERE c.repo_id = r.id AND c.user_id = $2) AS collaborator,
             EXISTS (SELECT 1 FROM users u WHERE u.id = $2 AND u.namespace_id = r.namespace_id) AS "namespace_owner!",
             (SELECT m.role FROM organizations o JOIN org_members m ON m.org_id = o.id
              WHERE o.namespace_id = r.namespace_id AND m.user_id = $2) AS org_role,
             (SELECT o.default_member_role FROM organizations o WHERE o.namespace_id = r.namespace_id) AS org_default_member_role
           FROM repositories r WHERE r.id = $1"#,
        repo.as_uuid(),
        user.as_uuid()
    )
    .fetch_optional(exec)
    .await?;
    Ok(r.map(|r| RoleFacts {
        collaborator: r.collaborator.and_then(|c| c.parse().ok()),
        namespace_owner: r.namespace_owner,
        org_role: r.org_role,
        org_default_member_role: r.org_default_member_role,
    })
    .unwrap_or_default())
}

/// Repositories in a namespace that a viewer can see (filtered in SQL),
/// newest activity first. `viewer` is `None` for anonymous visitors.
pub async fn list_visible_in_namespace(
    exec: impl PgExecutor<'_>,
    namespace: NamespaceId,
    viewer: Option<UserId>,
    site_admin: bool,
    limit: i64,
) -> Result<Vec<Repo>, DbError> {
    let viewer = viewer.map(|v| *v.as_uuid());
    let rows = sqlx::query!(
        r#"SELECT r.id, r.namespace_id, n.name::text AS "namespace!", n.kind AS namespace_kind, r.name::text AS "name!",
                  r.description, r.visibility, r.default_branch, r.state, r.allow_forking, r.size_bytes,
                  r.pushed_at, r.deleted_at, r.created_at, r.updated_at
           FROM repositories r JOIN namespaces n ON n.id = r.namespace_id WHERE r.namespace_id = $1 AND r.state <> 'deleting' AND (
                r.visibility = 'public' OR $3
                OR EXISTS (SELECT 1 FROM repo_collaborators c WHERE c.repo_id = r.id AND c.user_id = $2)
                OR EXISTS (SELECT 1 FROM users u WHERE u.id = $2 AND u.namespace_id = r.namespace_id)
                OR EXISTS (SELECT 1 FROM organizations o JOIN org_members m ON m.org_id = o.id
                           WHERE o.namespace_id = r.namespace_id AND m.user_id = $2
                             AND (m.role = 'owner' OR o.default_member_role <> 'none')))
             ORDER BY COALESCE(r.pushed_at, r.created_at) DESC LIMIT $4"#,
        namespace.as_uuid(),
        viewer,
        site_admin,
        limit,
    )
    .fetch_all(exec)
    .await?;
    Ok(rows.into_iter().map(|r| to_repo!(r)).collect())
}

/// Recently active public repositories (the explore page).
pub async fn list_public(exec: impl PgExecutor<'_>, limit: i64) -> Result<Vec<Repo>, DbError> {
    let rows = sqlx::query!(
        r#"SELECT r.id, r.namespace_id, n.name::text AS "namespace!", n.kind AS namespace_kind, r.name::text AS "name!",
                  r.description, r.visibility, r.default_branch, r.state, r.allow_forking, r.size_bytes,
                  r.pushed_at, r.deleted_at, r.created_at, r.updated_at
           FROM repositories r JOIN namespaces n ON n.id = r.namespace_id WHERE r.visibility = 'public' AND r.state <> 'deleting'
             ORDER BY COALESCE(r.pushed_at, r.created_at) DESC LIMIT $1"#,
        limit
    )
    .fetch_all(exec)
    .await?;
    Ok(rows.into_iter().map(|r| to_repo!(r)).collect())
}

pub async fn update_settings(
    exec: impl PgExecutor<'_>,
    id: RepoId,
    description: Option<&str>,
    default_branch: &str,
) -> Result<(), DbError> {
    sqlx::query!(
        "UPDATE repositories SET description = $2, default_branch = $3, updated_at = now() WHERE id = $1",
        id.as_uuid(),
        description,
        default_branch
    )
    .execute(exec)
    .await?;
    Ok(())
}

pub async fn set_visibility(
    exec: impl PgExecutor<'_>,
    id: RepoId,
    v: Visibility,
) -> Result<(), DbError> {
    sqlx::query!(
        "UPDATE repositories SET visibility = $2, updated_at = now() WHERE id = $1",
        id.as_uuid(),
        visibility_str(v)
    )
    .execute(exec)
    .await?;
    Ok(())
}

pub async fn set_state(
    exec: impl PgExecutor<'_>,
    id: RepoId,
    state: RepoState,
) -> Result<(), DbError> {
    let s = match state {
        RepoState::Active => "active",
        RepoState::Archived => "archived",
        RepoState::Deleting => "deleting",
    };
    sqlx::query!(
        "UPDATE repositories SET state = $2,
             archived_reason = CASE WHEN $2 = 'archived' THEN 'user' ELSE NULL END,
             deleted_at = CASE WHEN $2 = 'deleting' THEN now() ELSE NULL END,
             updated_at = now()
         WHERE id = $1",
        id.as_uuid(),
        s
    )
    .execute(exec)
    .await?;
    Ok(())
}

/// Renames a repository within its namespace and keeps the old name as a
/// redirect. Run in a transaction.
pub async fn rename(
    tx: &mut sqlx::PgConnection,
    id: RepoId,
    new_name: &str,
) -> Result<(), DbError> {
    let old = sqlx::query!(
        "SELECT namespace_id, name::text AS \"name!\" FROM repositories WHERE id = $1 FOR UPDATE",
        id.as_uuid()
    )
    .fetch_one(&mut *tx)
    .await?;
    sqlx::query!(
        "UPDATE repositories SET name = $2, updated_at = now() WHERE id = $1",
        id.as_uuid(),
        new_name
    )
    .execute(&mut *tx)
    .await
    .map_err(|e| DbError::from_unique(e, "name"))?;
    // The new name is a real repository now; an old redirect with that name goes.
    sqlx::query!(
        "DELETE FROM repo_redirects WHERE namespace_id = $1 AND name = $2::citext",
        old.namespace_id,
        new_name
    )
    .execute(&mut *tx)
    .await?;
    if !old.name.eq_ignore_ascii_case(new_name) {
        sqlx::query!(
            "INSERT INTO repo_redirects (namespace_id, name, repo_id) VALUES ($1, $2, $3)
             ON CONFLICT (namespace_id, name) DO UPDATE SET repo_id = EXCLUDED.repo_id, created_at = now()",
            old.namespace_id,
            old.name,
            id.as_uuid()
        )
        .execute(&mut *tx)
        .await?;
    }
    Ok(())
}

/// True if a repository (in any state) or redirect has this name.
pub async fn name_in_use(
    exec: impl PgExecutor<'_>,
    namespace: NamespaceId,
    name: &str,
) -> Result<bool, DbError> {
    Ok(sqlx::query_scalar!(
        r#"SELECT EXISTS (SELECT 1 FROM repositories WHERE namespace_id = $1 AND name = $2::citext) AS "e!""#,
        namespace.as_uuid(),
        name
    )
    .fetch_one(exec)
    .await?)
}

#[derive(Debug, Clone)]
pub struct Collaborator {
    pub user_id: UserId,
    pub name: String,
    pub role: Role,
}

pub async fn collaborators(
    exec: impl PgExecutor<'_>,
    repo: RepoId,
) -> Result<Vec<Collaborator>, DbError> {
    let rows = sqlx::query!(
        r#"SELECT c.user_id, n.name::text AS "name!", c.role FROM repo_collaborators c
           JOIN users u ON u.id = c.user_id JOIN namespaces n ON n.id = u.namespace_id
           WHERE c.repo_id = $1 ORDER BY n.name LIMIT 1000"#,
        repo.as_uuid()
    )
    .fetch_all(exec)
    .await?;
    Ok(rows
        .into_iter()
        .filter_map(|r| {
            Some(Collaborator {
                user_id: UserId::from_uuid(r.user_id),
                name: r.name,
                role: r.role.parse().ok()?,
            })
        })
        .collect())
}

pub async fn set_collaborator(
    exec: impl PgExecutor<'_>,
    repo: RepoId,
    user: UserId,
    role: Role,
) -> Result<(), DbError> {
    sqlx::query!(
        "INSERT INTO repo_collaborators (repo_id, user_id, role) VALUES ($1, $2, $3)
         ON CONFLICT (repo_id, user_id) DO UPDATE SET role = EXCLUDED.role",
        repo.as_uuid(),
        user.as_uuid(),
        role_str(role)
    )
    .execute(exec)
    .await?;
    Ok(())
}

pub async fn remove_collaborator(
    exec: impl PgExecutor<'_>,
    repo: RepoId,
    user: UserId,
) -> Result<bool, DbError> {
    let r = sqlx::query!(
        "DELETE FROM repo_collaborators WHERE repo_id = $1 AND user_id = $2",
        repo.as_uuid(),
        user.as_uuid()
    )
    .execute(exec)
    .await?;
    Ok(r.rows_affected() > 0)
}

/// Repositories being deleted whose trash retention has passed.
pub async fn deleted_before(
    exec: impl PgExecutor<'_>,
    cutoff: OffsetDateTime,
) -> Result<Vec<RepoId>, DbError> {
    let rows = sqlx::query_scalar!(
        "SELECT id FROM repositories WHERE state = 'deleting' AND deleted_at < $1 LIMIT 1000",
        cutoff
    )
    .fetch_all(exec)
    .await?;
    Ok(rows.into_iter().map(RepoId::from_uuid).collect())
}

/// Removes a purged repository's rows (Git data is gone already).
pub async fn purge(exec: impl PgExecutor<'_>, id: RepoId) -> Result<(), DbError> {
    sqlx::query!(
        "DELETE FROM repositories WHERE id = $1 AND state = 'deleting'",
        id.as_uuid()
    )
    .execute(exec)
    .await?;
    Ok(())
}

/// Repositories in the trash (for administrators).
pub async fn list_deleted(exec: impl PgExecutor<'_>, limit: i64) -> Result<Vec<Repo>, DbError> {
    let rows = sqlx::query!(r#"SELECT r.id, r.namespace_id, n.name::text AS "namespace!", n.kind AS namespace_kind, r.name::text AS "name!",
                  r.description, r.visibility, r.default_branch, r.state, r.allow_forking, r.size_bytes,
                  r.pushed_at, r.deleted_at, r.created_at, r.updated_at
           FROM repositories r JOIN namespaces n ON n.id = r.namespace_id WHERE r.state = 'deleting' ORDER BY r.deleted_at DESC LIMIT $1"#, limit)
        .fetch_all(exec)
        .await?;
    Ok(rows.into_iter().map(|r| to_repo!(r)).collect())
}

/// Every repository ID (any state).
pub async fn all_ids(exec: impl PgExecutor<'_>) -> Result<Vec<RepoId>, DbError> {
    let rows = sqlx::query_scalar!("SELECT id FROM repositories ORDER BY id")
        .fetch_all(exec)
        .await?;
    Ok(rows.into_iter().map(RepoId::from_uuid).collect())
}

/// Live repositories pushed to since `since` (all live ones for `None`).
pub async fn active_ids_pushed_since(
    exec: impl PgExecutor<'_>,
    since: Option<OffsetDateTime>,
) -> Result<Vec<RepoId>, DbError> {
    let rows = sqlx::query_scalar!(
        "SELECT id FROM repositories WHERE state <> 'deleting' AND ($1::timestamptz IS NULL OR pushed_at >= $1) ORDER BY id",
        since
    )
    .fetch_all(exec)
    .await?;
    Ok(rows.into_iter().map(RepoId::from_uuid).collect())
}
