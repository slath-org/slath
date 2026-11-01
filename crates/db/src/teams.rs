// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Teams of an organization, their members, and their repository access.

use slath_core::authz::Role;
use slath_core::ids::{OrgId, RepoId, UserId};
use sqlx::PgExecutor;
use time::OffsetDateTime;

use crate::DbError;
use crate::repos::role_str;

#[derive(Debug, Clone)]
pub struct Team {
    pub id: uuid::Uuid,
    pub org_id: OrgId,
    pub name: String,
    pub description: Option<String>,
    pub members: i64,
    pub repos: i64,
    pub created_at: OffsetDateTime,
}

pub async fn create(
    exec: impl PgExecutor<'_>,
    org: OrgId,
    name: &str,
    description: Option<&str>,
) -> Result<uuid::Uuid, DbError> {
    let id = uuid::Uuid::now_v7();
    sqlx::query!(
        "INSERT INTO teams (id, org_id, name, description) VALUES ($1, $2, $3, $4)",
        id,
        org.as_uuid(),
        name,
        description
    )
    .execute(exec)
    .await
    .map_err(|e| DbError::from_unique(e, "name"))?;
    Ok(id)
}

pub async fn list(exec: impl PgExecutor<'_>, org: OrgId) -> Result<Vec<Team>, DbError> {
    let rows = sqlx::query!(
        r#"SELECT t.id, t.org_id, t.name::text AS "name!", t.description, t.created_at,
                  (SELECT count(*) FROM team_members m WHERE m.team_id = t.id) AS "members!",
                  (SELECT count(*) FROM repo_team_access a WHERE a.team_id = t.id) AS "repos!"
           FROM teams t WHERE t.org_id = $1 ORDER BY lower(t.name::text)"#,
        org.as_uuid()
    )
    .fetch_all(exec)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| Team {
            id: r.id,
            org_id: OrgId::from_uuid(r.org_id),
            name: r.name,
            description: r.description,
            members: r.members,
            repos: r.repos,
            created_at: r.created_at,
        })
        .collect())
}

pub async fn find(
    exec: impl PgExecutor<'_>,
    org: OrgId,
    name: &str,
) -> Result<Option<Team>, DbError> {
    let r = sqlx::query!(
        r#"SELECT t.id, t.org_id, t.name::text AS "name!", t.description, t.created_at,
                  (SELECT count(*) FROM team_members m WHERE m.team_id = t.id) AS "members!",
                  (SELECT count(*) FROM repo_team_access a WHERE a.team_id = t.id) AS "repos!"
           FROM teams t WHERE t.org_id = $1 AND t.name = $2::text::citext"#,
        org.as_uuid(),
        name
    )
    .fetch_optional(exec)
    .await?;
    Ok(r.map(|r| Team {
        id: r.id,
        org_id: OrgId::from_uuid(r.org_id),
        name: r.name,
        description: r.description,
        members: r.members,
        repos: r.repos,
        created_at: r.created_at,
    }))
}

pub async fn delete(exec: impl PgExecutor<'_>, team: uuid::Uuid) -> Result<bool, DbError> {
    let done = sqlx::query!("DELETE FROM teams WHERE id = $1", team)
        .execute(exec)
        .await?;
    Ok(done.rows_affected() == 1)
}

/// Members as `(user id, name)`.
pub async fn members(
    exec: impl PgExecutor<'_>,
    team: uuid::Uuid,
) -> Result<Vec<(UserId, String)>, DbError> {
    let rows = sqlx::query!(
        r#"SELECT m.user_id, n.name::text AS "name!" FROM team_members m
           JOIN users u ON u.id = m.user_id JOIN namespaces n ON n.id = u.namespace_id
           WHERE m.team_id = $1 ORDER BY lower(n.name::text)"#,
        team
    )
    .fetch_all(exec)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| (UserId::from_uuid(r.user_id), r.name))
        .collect())
}

pub async fn add_member(
    exec: impl PgExecutor<'_>,
    team: uuid::Uuid,
    user: UserId,
) -> Result<(), DbError> {
    sqlx::query!(
        "INSERT INTO team_members (team_id, user_id) VALUES ($1, $2) ON CONFLICT DO NOTHING",
        team,
        user.as_uuid()
    )
    .execute(exec)
    .await?;
    Ok(())
}

pub async fn remove_member(
    exec: impl PgExecutor<'_>,
    team: uuid::Uuid,
    user: UserId,
) -> Result<bool, DbError> {
    let done = sqlx::query!(
        "DELETE FROM team_members WHERE team_id = $1 AND user_id = $2",
        team,
        user.as_uuid()
    )
    .execute(exec)
    .await?;
    Ok(done.rows_affected() == 1)
}

/// Removes a user from every team of an organization (leaving it).
pub async fn remove_from_org_teams(
    exec: impl PgExecutor<'_>,
    org: OrgId,
    user: UserId,
) -> Result<(), DbError> {
    sqlx::query!(
        "DELETE FROM team_members WHERE user_id = $2 AND team_id IN (SELECT id FROM teams WHERE org_id = $1)",
        org.as_uuid(),
        user.as_uuid()
    )
    .execute(exec)
    .await?;
    Ok(())
}

/// A team's repositories as `(repo id, full name, role)`.
pub async fn repos(
    exec: impl PgExecutor<'_>,
    team: uuid::Uuid,
) -> Result<Vec<(RepoId, String, Role)>, DbError> {
    let rows = sqlx::query!(
        r#"SELECT a.repo_id, n.name::text || '/' || r.name::text AS "full!", a.role FROM repo_team_access a
           JOIN repositories r ON r.id = a.repo_id JOIN namespaces n ON n.id = r.namespace_id
           WHERE a.team_id = $1 AND r.state <> 'deleting' ORDER BY 2"#,
        team
    )
    .fetch_all(exec)
    .await?;
    Ok(rows
        .into_iter()
        .filter_map(|r| Some((RepoId::from_uuid(r.repo_id), r.full, r.role.parse().ok()?)))
        .collect())
}

/// Teams with access to a repository as `(team name, role)`.
pub async fn repo_teams(
    exec: impl PgExecutor<'_>,
    repo: RepoId,
) -> Result<Vec<(String, Role)>, DbError> {
    let rows = sqlx::query!(
        r#"SELECT t.name::text AS "name!", a.role FROM repo_team_access a JOIN teams t ON t.id = a.team_id
           WHERE a.repo_id = $1 ORDER BY lower(t.name::text)"#,
        repo.as_uuid()
    )
    .fetch_all(exec)
    .await?;
    Ok(rows
        .into_iter()
        .filter_map(|r| Some((r.name, r.role.parse().ok()?)))
        .collect())
}

pub async fn set_repo_access(
    exec: impl PgExecutor<'_>,
    repo: RepoId,
    team: uuid::Uuid,
    role: Role,
) -> Result<(), DbError> {
    sqlx::query!(
        "INSERT INTO repo_team_access (repo_id, team_id, role) VALUES ($1, $2, $3)
         ON CONFLICT (repo_id, team_id) DO UPDATE SET role = EXCLUDED.role",
        repo.as_uuid(),
        team,
        role_str(role)
    )
    .execute(exec)
    .await?;
    Ok(())
}

pub async fn remove_repo_access(
    exec: impl PgExecutor<'_>,
    repo: RepoId,
    team: uuid::Uuid,
) -> Result<bool, DbError> {
    let done = sqlx::query!(
        "DELETE FROM repo_team_access WHERE repo_id = $1 AND team_id = $2",
        repo.as_uuid(),
        team
    )
    .execute(exec)
    .await?;
    Ok(done.rows_affected() == 1)
}
