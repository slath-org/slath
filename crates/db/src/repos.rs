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
    pub issues_enabled: bool,
    pub pushed_at: Option<OffsetDateTime>,
    pub deleted_at: Option<OffsetDateTime>,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}

pub(crate) fn visibility(s: &str) -> Visibility {
    match s {
        "public" => Visibility::Public,
        "internal" => Visibility::Internal,
        _ => Visibility::Private,
    }
}

pub(crate) fn repo_state(s: &str) -> RepoState {
    match s {
        "active" => RepoState::Active,
        "archived" => RepoState::Archived,
        "locked" => RepoState::Locked,
        _ => RepoState::Deleting,
    }
}

#[must_use]
pub const fn visibility_str(v: Visibility) -> &'static str {
    match v {
        Visibility::Public => "public",
        Visibility::Internal => "internal",
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
            issues_enabled: $r.issues_enabled,
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
                  r.pushed_at, r.deleted_at, r.created_at, r.updated_at, r.issues_enabled
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
                  r.pushed_at, r.deleted_at, r.created_at, r.updated_at, r.issues_enabled
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
    /// Roles granted to teams the user is in.
    pub team_roles: Vec<Role>,
    /// The owning organization requires MFA, which the user doesn't have.
    pub mfa_unmet: bool,
}

pub async fn role_facts(
    exec: impl PgExecutor<'_>,
    repo: RepoId,
    user: UserId,
) -> Result<RoleFacts, DbError> {
    let r = sqlx::query!(
        r#"SELECT
             (SELECT c.role FROM repo_collaborators c WHERE c.repo_id = r.id AND c.user_id = $2) AS collaborator,
             slath_owns_namespace(r.namespace_id, $2) AS "namespace_owner!",
             (SELECT m.role FROM organizations o JOIN org_members m ON m.org_id = o.id
              WHERE o.namespace_id = r.namespace_id AND m.user_id = $2) AS org_role,
             (SELECT o.default_member_role FROM organizations o WHERE o.namespace_id = r.namespace_id) AS org_default_member_role,
             ARRAY(SELECT a.role FROM repo_team_access a JOIN team_members tm ON tm.team_id = a.team_id
                   WHERE a.repo_id = r.id AND tm.user_id = $2) AS "team_roles!",
             COALESCE((SELECT o.require_mfa FROM organizations o WHERE o.namespace_id = r.namespace_id), false)
               AND NOT slath_has_mfa($2) AS "mfa_unmet!"
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
        team_roles: r.team_roles.iter().filter_map(|t| t.parse().ok()).collect(),
        mfa_unmet: r.mfa_unmet,
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
                  r.pushed_at, r.deleted_at, r.created_at, r.updated_at, r.issues_enabled
           FROM repositories r JOIN namespaces n ON n.id = r.namespace_id WHERE r.namespace_id = $1 AND r.state <> 'deleting' AND (
                r.visibility = 'public' OR $3
                OR (r.visibility = 'internal' AND $2::uuid IS NOT NULL)
                OR slath_can_read(r.id, $2::uuid))
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
                  r.pushed_at, r.deleted_at, r.created_at, r.updated_at, r.issues_enabled
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
        RepoState::Locked => "locked",
    };
    sqlx::query!(
        "UPDATE repositories SET state = $2,
             archived_reason = CASE WHEN $2 = 'archived' THEN 'user' ELSE NULL END,
             deleted_at = CASE WHEN $2 IN ('deleting', 'locked') THEN now() ELSE NULL END,
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

/// Repositories being deleted, and locked forks, whose retention has passed.
pub async fn deleted_before(
    exec: impl PgExecutor<'_>,
    cutoff: OffsetDateTime,
) -> Result<Vec<RepoId>, DbError> {
    let rows = sqlx::query_scalar!(
        "SELECT id FROM repositories WHERE state IN ('deleting', 'locked') AND deleted_at < $1 LIMIT 1000",
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
                  r.pushed_at, r.deleted_at, r.created_at, r.updated_at, r.issues_enabled
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

/// Every repository's default branch.
pub async fn default_branches(exec: impl PgExecutor<'_>) -> Result<Vec<(RepoId, String)>, DbError> {
    let rows = sqlx::query!("SELECT id, default_branch FROM repositories ORDER BY id")
        .fetch_all(exec)
        .await?;
    Ok(rows
        .into_iter()
        .map(|r| (RepoId::from_uuid(r.id), r.default_branch))
        .collect())
}

/// Entity counts for backup and restore summaries.
pub async fn entity_counts(pool: &sqlx::PgPool) -> Result<Vec<(&'static str, u64)>, DbError> {
    let r = sqlx::query!(
        r#"SELECT (SELECT count(*) FROM users) AS "users!",
                  (SELECT count(*) FROM organizations) AS "organizations!",
                  (SELECT count(*) FROM repositories) AS "repositories!",
                  (SELECT count(*) FROM git_refs) AS "refs!""#
    )
    .fetch_one(pool)
    .await?;
    let n = |v: i64| u64::try_from(v).unwrap_or(0);
    Ok(vec![
        ("users", n(r.users)),
        ("organizations", n(r.organizations)),
        ("repositories", n(r.repositories)),
        ("refs", n(r.refs)),
    ])
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

/// Repositories a viewer can see in any namespace (filtered in SQL),
/// ordered by full name. `viewer` is `None` for anonymous visitors.
pub async fn list_visible(
    exec: impl PgExecutor<'_>,
    viewer: Option<UserId>,
    site_admin: bool,
    offset: i64,
    limit: i64,
) -> Result<Vec<Repo>, DbError> {
    let viewer = viewer.map(|v| *v.as_uuid());
    let rows = sqlx::query!(
        r#"SELECT r.id, r.namespace_id, n.name::text AS "namespace!", n.kind AS namespace_kind, r.name::text AS "name!",
                  r.description, r.visibility, r.default_branch, r.state, r.allow_forking, r.size_bytes,
                  r.pushed_at, r.deleted_at, r.created_at, r.updated_at, r.issues_enabled
           FROM repositories r JOIN namespaces n ON n.id = r.namespace_id
           WHERE r.state <> 'deleting' AND (
                r.visibility = 'public' OR $2
                OR (r.visibility = 'internal' AND $1::uuid IS NOT NULL)
                OR slath_can_read(r.id, $1::uuid))
           ORDER BY lower(n.name::text), lower(r.name::text) OFFSET $3 LIMIT $4"#,
        viewer,
        site_admin,
        offset,
        limit,
    )
    .fetch_all(exec)
    .await?;
    Ok(rows.into_iter().map(|r| to_repo!(r)).collect())
}

/// All repositories in any state, by full name (administrators).
pub async fn list_all(
    exec: impl PgExecutor<'_>,
    offset: i64,
    limit: i64,
) -> Result<Vec<Repo>, DbError> {
    let rows = sqlx::query!(
        r#"SELECT r.id, r.namespace_id, n.name::text AS "namespace!", n.kind AS namespace_kind, r.name::text AS "name!",
                  r.description, r.visibility, r.default_branch, r.state, r.allow_forking, r.size_bytes,
                  r.pushed_at, r.deleted_at, r.created_at, r.updated_at, r.issues_enabled
           FROM repositories r JOIN namespaces n ON n.id = r.namespace_id
           ORDER BY lower(n.name::text), lower(r.name::text) OFFSET $1 LIMIT $2"#,
        offset,
        limit
    )
    .fetch_all(exec)
    .await?;
    Ok(rows.into_iter().map(|r| to_repo!(r)).collect())
}

/// Counts a push toward the next maintenance run. Returns the pushes since
/// the last run and the number of completed runs.
pub async fn count_push_for_maintenance(
    exec: impl PgExecutor<'_>,
    id: RepoId,
) -> Result<(i32, i32), DbError> {
    let r = sqlx::query!(
        "UPDATE repositories SET pushes_since_maintenance = pushes_since_maintenance + 1
         WHERE id = $1 RETURNING pushes_since_maintenance, maintenance_runs",
        id.as_uuid()
    )
    .fetch_optional(exec)
    .await?;
    Ok(r.map_or((0, 0), |r| (r.pushes_since_maintenance, r.maintenance_runs)))
}

/// Repositories with pushes whose last maintenance (or creation) is older
/// than `before`, with their run counters.
pub async fn maintenance_due(
    exec: impl PgExecutor<'_>,
    before: OffsetDateTime,
) -> Result<Vec<(RepoId, i32)>, DbError> {
    let rows = sqlx::query!(
        "SELECT id, maintenance_runs FROM repositories
         WHERE pushes_since_maintenance > 0 AND state <> 'deleting'
           AND coalesce(maintained_at, created_at) < $1
         ORDER BY coalesce(maintained_at, created_at) LIMIT 1000",
        before
    )
    .fetch_all(exec)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| (RepoId::from_uuid(r.id), r.maintenance_runs))
        .collect())
}

/// Takes the maintenance lease of a live repository for `seconds`. Returns
/// the pushes counted so far, or `None` if another run holds the lease or
/// the repository is gone.
pub async fn take_maintenance_lease(
    exec: impl PgExecutor<'_>,
    id: RepoId,
    seconds: u32,
) -> Result<Option<i32>, DbError> {
    Ok(sqlx::query_scalar!(
        "UPDATE repositories SET maintenance_lease_until = now() + make_interval(secs => $2)
         WHERE id = $1 AND state <> 'deleting'
           AND (maintenance_lease_until IS NULL OR maintenance_lease_until < now())
         RETURNING pushes_since_maintenance",
        id.as_uuid(),
        f64::from(seconds)
    )
    .fetch_optional(exec)
    .await?)
}

/// Records a finished maintenance run and releases the lease. Pushes that
/// arrived during the run stay counted.
pub async fn finish_maintenance(
    exec: impl PgExecutor<'_>,
    id: RepoId,
    counted: i32,
    size_bytes: Option<i64>,
) -> Result<(), DbError> {
    sqlx::query!(
        "UPDATE repositories SET maintained_at = now(),
                maintenance_runs = maintenance_runs + 1,
                pushes_since_maintenance = greatest(pushes_since_maintenance - $2, 0),
                size_bytes = coalesce($3, size_bytes),
                maintenance_lease_until = NULL
         WHERE id = $1",
        id.as_uuid(),
        counted,
        size_bytes
    )
    .execute(exec)
    .await?;
    Ok(())
}

/// Stores a repository's measured size.
pub async fn set_size(
    exec: impl PgExecutor<'_>,
    id: RepoId,
    size_bytes: i64,
) -> Result<(), DbError> {
    sqlx::query!(
        "UPDATE repositories SET size_bytes = $2 WHERE id = $1",
        id.as_uuid(),
        size_bytes
    )
    .execute(exec)
    .await?;
    Ok(())
}

/// Whether the repository has issues enabled.
pub async fn issues_enabled(exec: impl PgExecutor<'_>, id: RepoId) -> Result<bool, DbError> {
    let r = sqlx::query_scalar!(
        "SELECT issues_enabled FROM repositories WHERE id = $1",
        id.as_uuid()
    )
    .fetch_optional(exec)
    .await?;
    Ok(r.unwrap_or(false))
}

/// A repository's push protection mode (`block`, `warn`, or `off`).
pub async fn push_protection(exec: impl PgExecutor<'_>, id: RepoId) -> Result<String, DbError> {
    Ok(sqlx::query_scalar!(
        "SELECT push_protection FROM repositories WHERE id = $1",
        id.as_uuid()
    )
    .fetch_one(exec)
    .await?)
}

pub async fn set_push_protection(
    exec: impl PgExecutor<'_>,
    id: RepoId,
    mode: &str,
) -> Result<(), DbError> {
    sqlx::query!(
        "UPDATE repositories SET push_protection = $2, updated_at = now() WHERE id = $1",
        id.as_uuid(),
        mode
    )
    .execute(exec)
    .await?;
    Ok(())
}

/// Who may allow push protection findings: `write` or `admin`.
pub async fn secret_allowance_role(
    exec: impl PgExecutor<'_>,
    id: RepoId,
) -> Result<String, DbError> {
    Ok(sqlx::query_scalar!(
        "SELECT secret_allowance_role FROM repositories WHERE id = $1",
        id.as_uuid()
    )
    .fetch_one(exec)
    .await?)
}

pub async fn set_secret_allowance_role(
    exec: impl PgExecutor<'_>,
    id: RepoId,
    role: &str,
) -> Result<(), DbError> {
    sqlx::query!(
        "UPDATE repositories SET secret_allowance_role = $2, updated_at = now() WHERE id = $1",
        id.as_uuid(),
        role
    )
    .execute(exec)
    .await?;
    Ok(())
}

/// Whether merge requests are enabled, and the default for deleting the
/// source branch after merging.
pub async fn mr_settings(exec: impl PgExecutor<'_>, id: RepoId) -> Result<(bool, bool), DbError> {
    let r = sqlx::query!(
        "SELECT merge_requests_enabled, delete_source_branch_default FROM repositories WHERE id = $1",
        id.as_uuid()
    )
    .fetch_optional(exec)
    .await?;
    Ok(r.map_or((false, false), |r| {
        (r.merge_requests_enabled, r.delete_source_branch_default)
    }))
}

/// The merge strategies a repository allows, and its default.
pub async fn merge_settings(
    exec: impl PgExecutor<'_>,
    id: RepoId,
) -> Result<(Vec<String>, String), DbError> {
    let r = sqlx::query!(
        "SELECT merge_strategies, default_merge_strategy FROM repositories WHERE id = $1",
        id.as_uuid()
    )
    .fetch_one(exec)
    .await?;
    Ok((r.merge_strategies, r.default_merge_strategy))
}
