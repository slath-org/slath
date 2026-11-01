// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Labels, milestones, and assignees of issues.

use slath_core::ids::{RepoId, UserId};
use sqlx::PgExecutor;
use time::{Date, OffsetDateTime};

use crate::DbError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Label {
    pub id: uuid::Uuid,
    pub name: String,
    /// Six lowercase hex digits.
    pub color: String,
    pub description: Option<String>,
    /// Open issues with the label.
    pub open_issues: i64,
}

pub async fn labels(exec: impl PgExecutor<'_>, repo: RepoId) -> Result<Vec<Label>, DbError> {
    let rows = sqlx::query_as!(
        Label,
        r#"SELECT l.id, l.name::text AS "name!", l.color::text AS "color!", l.description,
                  (SELECT count(*) FROM issue_labels il JOIN issues i ON i.id = il.issue_id
                   WHERE il.label_id = l.id AND i.state = 'open') AS "open_issues!"
           FROM labels l WHERE l.repo_id = $1 ORDER BY lower(l.name::text)"#,
        repo.as_uuid()
    )
    .fetch_all(exec)
    .await?;
    Ok(rows)
}

pub async fn create_label(
    exec: impl PgExecutor<'_>,
    repo: RepoId,
    name: &str,
    color: &str,
    description: Option<&str>,
) -> Result<uuid::Uuid, DbError> {
    let id = uuid::Uuid::now_v7();
    sqlx::query!(
        "INSERT INTO labels (id, repo_id, name, color, description) VALUES ($1, $2, $3, $4, $5)",
        id,
        repo.as_uuid(),
        name,
        color,
        description
    )
    .execute(exec)
    .await
    .map_err(|e| DbError::from_unique(e, "name"))?;
    Ok(id)
}

pub async fn update_label(
    exec: impl PgExecutor<'_>,
    repo: RepoId,
    id: uuid::Uuid,
    name: &str,
    color: &str,
    description: Option<&str>,
) -> Result<bool, DbError> {
    let done = sqlx::query!(
        "UPDATE labels SET name = $3, color = $4, description = $5 WHERE id = $1 AND repo_id = $2",
        id,
        repo.as_uuid(),
        name,
        color,
        description
    )
    .execute(exec)
    .await
    .map_err(|e| DbError::from_unique(e, "name"))?;
    Ok(done.rows_affected() == 1)
}

pub async fn delete_label(
    exec: impl PgExecutor<'_>,
    repo: RepoId,
    id: uuid::Uuid,
) -> Result<bool, DbError> {
    let done = sqlx::query!(
        "DELETE FROM labels WHERE id = $1 AND repo_id = $2",
        id,
        repo.as_uuid()
    )
    .execute(exec)
    .await?;
    Ok(done.rows_affected() == 1)
}

/// Labels by name (case-insensitive) in a repository.
pub async fn labels_by_name(
    exec: impl PgExecutor<'_>,
    repo: RepoId,
    names: &[String],
) -> Result<Vec<Label>, DbError> {
    let rows = sqlx::query_as!(
        Label,
        r#"SELECT l.id, l.name::text AS "name!", l.color::text AS "color!", l.description, 0::bigint AS "open_issues!"
           FROM labels l WHERE l.repo_id = $1 AND lower(l.name::text) = ANY(SELECT lower(x) FROM unnest($2::text[]) x)"#,
        repo.as_uuid(),
        names
    )
    .fetch_all(exec)
    .await?;
    Ok(rows)
}

/// The labels of issues, as `(issue id, label)`.
pub async fn issue_labels(
    exec: impl PgExecutor<'_>,
    issues: &[uuid::Uuid],
) -> Result<Vec<(uuid::Uuid, Label)>, DbError> {
    let rows = sqlx::query!(
        r#"SELECT il.issue_id, l.id, l.name::text AS "name!", l.color::text AS "color!", l.description
           FROM issue_labels il JOIN labels l ON l.id = il.label_id
           WHERE il.issue_id = ANY($1) ORDER BY lower(l.name::text)"#,
        issues
    )
    .fetch_all(exec)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| {
            (
                r.issue_id,
                Label {
                    id: r.id,
                    name: r.name,
                    color: r.color,
                    description: r.description,
                    open_issues: 0,
                },
            )
        })
        .collect())
}

pub async fn add_issue_label(
    exec: impl PgExecutor<'_>,
    issue: uuid::Uuid,
    label: uuid::Uuid,
) -> Result<bool, DbError> {
    let done = sqlx::query!(
        "INSERT INTO issue_labels (issue_id, label_id) VALUES ($1, $2) ON CONFLICT DO NOTHING",
        issue,
        label
    )
    .execute(exec)
    .await?;
    Ok(done.rows_affected() == 1)
}

pub async fn remove_issue_label(
    exec: impl PgExecutor<'_>,
    issue: uuid::Uuid,
    label: uuid::Uuid,
) -> Result<bool, DbError> {
    let done = sqlx::query!(
        "DELETE FROM issue_labels WHERE issue_id = $1 AND label_id = $2",
        issue,
        label
    )
    .execute(exec)
    .await?;
    Ok(done.rows_affected() == 1)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Milestone {
    pub id: uuid::Uuid,
    pub title: String,
    pub description: Option<String>,
    pub due_on: Option<Date>,
    pub state: String,
    pub open_issues: i64,
    pub closed_issues: i64,
    pub updated_at: OffsetDateTime,
}

pub async fn milestones(
    exec: impl PgExecutor<'_>,
    repo: RepoId,
) -> Result<Vec<Milestone>, DbError> {
    let rows = sqlx::query_as!(
        Milestone,
        r#"SELECT m.id, m.title::text AS "title!", m.description, m.due_on, m.state, m.updated_at,
                  (SELECT count(*) FROM issues i WHERE i.milestone_id = m.id AND i.state = 'open') AS "open_issues!",
                  (SELECT count(*) FROM issues i WHERE i.milestone_id = m.id AND i.state = 'closed') AS "closed_issues!"
           FROM milestones m WHERE m.repo_id = $1
           ORDER BY m.state DESC, m.due_on NULLS LAST, lower(m.title::text)"#,
        repo.as_uuid()
    )
    .fetch_all(exec)
    .await?;
    Ok(rows)
}

pub async fn create_milestone(
    exec: impl PgExecutor<'_>,
    repo: RepoId,
    title: &str,
    description: Option<&str>,
    due_on: Option<Date>,
) -> Result<uuid::Uuid, DbError> {
    let id = uuid::Uuid::now_v7();
    sqlx::query!(
        "INSERT INTO milestones (id, repo_id, title, description, due_on) VALUES ($1, $2, $3, $4, $5)",
        id,
        repo.as_uuid(),
        title,
        description,
        due_on
    )
    .execute(exec)
    .await
    .map_err(|e| DbError::from_unique(e, "title"))?;
    Ok(id)
}

pub async fn update_milestone(
    exec: impl PgExecutor<'_>,
    repo: RepoId,
    id: uuid::Uuid,
    title: &str,
    description: Option<&str>,
    due_on: Option<Date>,
    open: bool,
) -> Result<bool, DbError> {
    let done = sqlx::query!(
        "UPDATE milestones SET title = $3, description = $4, due_on = $5,
             state = CASE WHEN $6 THEN 'open' ELSE 'closed' END, updated_at = now()
         WHERE id = $1 AND repo_id = $2",
        id,
        repo.as_uuid(),
        title,
        description,
        due_on,
        open
    )
    .execute(exec)
    .await
    .map_err(|e| DbError::from_unique(e, "title"))?;
    Ok(done.rows_affected() == 1)
}

pub async fn delete_milestone(
    exec: impl PgExecutor<'_>,
    repo: RepoId,
    id: uuid::Uuid,
) -> Result<bool, DbError> {
    let done = sqlx::query!(
        "DELETE FROM milestones WHERE id = $1 AND repo_id = $2",
        id,
        repo.as_uuid()
    )
    .execute(exec)
    .await?;
    Ok(done.rows_affected() == 1)
}

pub async fn set_issue_milestone(
    exec: impl PgExecutor<'_>,
    issue: uuid::Uuid,
    milestone: Option<uuid::Uuid>,
) -> Result<(), DbError> {
    sqlx::query!(
        "UPDATE issues SET milestone_id = $2, updated_at = now() WHERE id = $1",
        issue,
        milestone
    )
    .execute(exec)
    .await?;
    Ok(())
}

/// The assignees of issues, as `(issue id, user id, name)`.
pub async fn assignees(
    exec: impl PgExecutor<'_>,
    issues: &[uuid::Uuid],
) -> Result<Vec<(uuid::Uuid, UserId, String)>, DbError> {
    let rows = sqlx::query!(
        r#"SELECT a.issue_id, a.user_id, n.name::text AS "name!" FROM issue_assignees a
           JOIN users u ON u.id = a.user_id JOIN namespaces n ON n.id = u.namespace_id
           WHERE a.issue_id = ANY($1) ORDER BY lower(n.name::text)"#,
        issues
    )
    .fetch_all(exec)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| (r.issue_id, UserId::from_uuid(r.user_id), r.name))
        .collect())
}

pub async fn add_assignee(
    exec: impl PgExecutor<'_>,
    issue: uuid::Uuid,
    user: UserId,
) -> Result<bool, DbError> {
    let done = sqlx::query!(
        "INSERT INTO issue_assignees (issue_id, user_id) VALUES ($1, $2) ON CONFLICT DO NOTHING",
        issue,
        user.as_uuid()
    )
    .execute(exec)
    .await?;
    Ok(done.rows_affected() == 1)
}

pub async fn remove_assignee(
    exec: impl PgExecutor<'_>,
    issue: uuid::Uuid,
    user: UserId,
) -> Result<bool, DbError> {
    let done = sqlx::query!(
        "DELETE FROM issue_assignees WHERE issue_id = $1 AND user_id = $2",
        issue,
        user.as_uuid()
    )
    .execute(exec)
    .await?;
    Ok(done.rows_affected() == 1)
}
