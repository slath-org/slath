// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Issues, comments, and timeline events.

use serde_json::Value;
use slath_core::ids::{RepoId, UserId};
use sqlx::PgExecutor;
use time::OffsetDateTime;

use crate::DbError;

#[derive(Debug, Clone)]
pub struct Issue {
    pub id: uuid::Uuid,
    pub repo_id: RepoId,
    pub number: i32,
    pub author_id: Option<UserId>,
    /// `None` for deleted accounts.
    pub author: Option<String>,
    pub title: String,
    pub body: String,
    pub state: String,
    pub state_reason: Option<String>,
    pub milestone_id: Option<uuid::Uuid>,
    pub comments: i64,
    pub closed_at: Option<OffsetDateTime>,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}

macro_rules! to_issue {
    ($r:expr) => {
        Issue {
            id: $r.id,
            repo_id: RepoId::from_uuid($r.repo_id),
            number: $r.number,
            author_id: $r.author_id.map(UserId::from_uuid),
            author: $r.author,
            title: $r.title,
            body: $r.body,
            state: $r.state,
            state_reason: $r.state_reason,
            milestone_id: $r.milestone_id,
            comments: $r.comments,
            closed_at: $r.closed_at,
            created_at: $r.created_at,
            updated_at: $r.updated_at,
        }
    };
}

/// Takes the next item number of a repository (shared by issues and merge
/// requests). Call inside the transaction that inserts the item: the row
/// lock serializes concurrent creations, so numbers are never reused.
pub async fn next_number(tx: &mut sqlx::PgConnection, repo: RepoId) -> Result<i32, DbError> {
    let n = sqlx::query_scalar!(
        "UPDATE repositories SET next_item_number = next_item_number + 1 WHERE id = $1
         RETURNING next_item_number - 1 AS \"n!\"",
        repo.as_uuid()
    )
    .fetch_one(&mut *tx)
    .await?;
    Ok(n)
}

pub async fn insert(
    tx: &mut sqlx::PgConnection,
    repo: RepoId,
    number: i32,
    author: UserId,
    title: &str,
    body: &str,
) -> Result<uuid::Uuid, DbError> {
    let id = uuid::Uuid::now_v7();
    sqlx::query!(
        "INSERT INTO issues (id, repo_id, number, author_id, title, body) VALUES ($1, $2, $3, $4, $5, $6)",
        id,
        repo.as_uuid(),
        number,
        author.as_uuid(),
        title,
        body
    )
    .execute(&mut *tx)
    .await?;
    Ok(id)
}

pub async fn get(
    exec: impl PgExecutor<'_>,
    repo: RepoId,
    number: i32,
) -> Result<Option<Issue>, DbError> {
    let r = sqlx::query!(
        r#"SELECT i.id, i.repo_id, i.number, i.author_id, n.name::text AS author, i.title, i.body, i.state,
                  i.state_reason, i.milestone_id, i.closed_at, i.created_at, i.updated_at,
                  (SELECT count(*) FROM comments c WHERE c.subject_kind = 'issue' AND c.subject_id = i.id
                     AND c.deleted_at IS NULL) AS "comments!"
           FROM issues i LEFT JOIN users u ON u.id = i.author_id AND u.state <> 'deleted'
           LEFT JOIN namespaces n ON n.id = u.namespace_id
           WHERE i.repo_id = $1 AND i.number = $2"#,
        repo.as_uuid(),
        number
    )
    .fetch_optional(exec)
    .await?;
    Ok(r.map(|r| to_issue!(r)))
}

/// Filters of the issue list. Values combine with AND.
#[derive(Debug, Clone, Default)]
pub struct Filter {
    /// `open`, `closed`, or `None` for both.
    pub state: Option<String>,
    pub author: Option<UserId>,
    pub assignee: Option<UserId>,
    /// Every one of these labels.
    pub labels: Vec<uuid::Uuid>,
    pub milestone: Option<uuid::Uuid>,
    /// `created` (default) or `updated`, newest first.
    pub sort_updated: bool,
}

pub async fn list(
    exec: impl PgExecutor<'_>,
    repo: RepoId,
    f: &Filter,
    offset: i64,
    limit: i64,
) -> Result<Vec<Issue>, DbError> {
    let rows = sqlx::query!(
        r#"SELECT i.id, i.repo_id, i.number, i.author_id, n.name::text AS author, i.title, i.body, i.state,
                  i.state_reason, i.milestone_id, i.closed_at, i.created_at, i.updated_at,
                  (SELECT count(*) FROM comments c WHERE c.subject_kind = 'issue' AND c.subject_id = i.id
                     AND c.deleted_at IS NULL) AS "comments!"
           FROM issues i LEFT JOIN users u ON u.id = i.author_id AND u.state <> 'deleted'
           LEFT JOIN namespaces n ON n.id = u.namespace_id
           WHERE i.repo_id = $1
             AND ($2::text IS NULL OR i.state = $2)
             AND ($3::uuid IS NULL OR i.author_id = $3)
             AND ($4::uuid IS NULL OR EXISTS (SELECT 1 FROM issue_assignees a WHERE a.issue_id = i.id AND a.user_id = $4))
             AND (SELECT count(*) FROM issue_labels l WHERE l.issue_id = i.id AND l.label_id = ANY($5)) = cardinality($5)
             AND ($6::uuid IS NULL OR i.milestone_id = $6)
           ORDER BY CASE WHEN $7 THEN i.updated_at ELSE i.created_at END DESC, i.number DESC
           OFFSET $8 LIMIT $9"#,
        repo.as_uuid(),
        f.state,
        f.author.map(|u| *u.as_uuid()),
        f.assignee.map(|u| *u.as_uuid()),
        &f.labels,
        f.milestone,
        f.sort_updated,
        offset,
        limit
    )
    .fetch_all(exec)
    .await?;
    Ok(rows.into_iter().map(|r| to_issue!(r)).collect())
}

/// `(open, closed)` counts of the issues matching a filter (its state is
/// ignored).
pub async fn counts(
    exec: impl PgExecutor<'_>,
    repo: RepoId,
    f: &Filter,
) -> Result<(i64, i64), DbError> {
    let r = sqlx::query!(
        r#"SELECT count(*) FILTER (WHERE i.state = 'open') AS "open!", count(*) FILTER (WHERE i.state = 'closed') AS "closed!"
           FROM issues i WHERE i.repo_id = $1
             AND ($2::uuid IS NULL OR i.author_id = $2)
             AND ($3::uuid IS NULL OR EXISTS (SELECT 1 FROM issue_assignees a WHERE a.issue_id = i.id AND a.user_id = $3))
             AND (SELECT count(*) FROM issue_labels l WHERE l.issue_id = i.id AND l.label_id = ANY($4)) = cardinality($4)
             AND ($5::uuid IS NULL OR i.milestone_id = $5)"#,
        repo.as_uuid(),
        f.author.map(|u| *u.as_uuid()),
        f.assignee.map(|u| *u.as_uuid()),
        &f.labels,
        f.milestone
    )
    .fetch_one(exec)
    .await?;
    Ok((r.open, r.closed))
}

pub async fn update_text(
    exec: impl PgExecutor<'_>,
    id: uuid::Uuid,
    title: &str,
    body: &str,
) -> Result<(), DbError> {
    sqlx::query!(
        "UPDATE issues SET title = $2, body = $3, updated_at = now() WHERE id = $1",
        id,
        title,
        body
    )
    .execute(exec)
    .await?;
    Ok(())
}

/// Opens or closes; returns false if it already was in that state.
pub async fn set_state(
    exec: impl PgExecutor<'_>,
    id: uuid::Uuid,
    open: bool,
    reason: Option<&str>,
) -> Result<bool, DbError> {
    let done = sqlx::query!(
        "UPDATE issues SET state = CASE WHEN $2 THEN 'open' ELSE 'closed' END,
             state_reason = CASE WHEN $2 THEN NULL ELSE $3 END,
             closed_at = CASE WHEN $2 THEN NULL ELSE now() END, updated_at = now()
         WHERE id = $1 AND state <> CASE WHEN $2 THEN 'open' ELSE 'closed' END",
        id,
        open,
        reason
    )
    .execute(exec)
    .await?;
    Ok(done.rows_affected() == 1)
}

pub async fn touch(exec: impl PgExecutor<'_>, id: uuid::Uuid) -> Result<(), DbError> {
    sqlx::query!("UPDATE issues SET updated_at = now() WHERE id = $1", id)
        .execute(exec)
        .await?;
    Ok(())
}

/// `(number, title, state)` of issues by number (reference rendering).
pub async fn titles(
    exec: impl PgExecutor<'_>,
    repo: RepoId,
    numbers: &[i32],
) -> Result<Vec<(i32, String, String)>, DbError> {
    let rows = sqlx::query!(
        "SELECT number, title, state FROM issues WHERE repo_id = $1 AND number = ANY($2)",
        repo.as_uuid(),
        numbers
    )
    .fetch_all(exec)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| (r.number, r.title, r.state))
        .collect())
}

#[derive(Debug, Clone)]
pub struct Comment {
    pub id: uuid::Uuid,
    pub repo_id: RepoId,
    pub subject_id: uuid::Uuid,
    pub author_id: Option<UserId>,
    pub author: Option<String>,
    pub body: String,
    pub edited_at: Option<OffsetDateTime>,
    pub deleted_at: Option<OffsetDateTime>,
    pub hidden_at: Option<OffsetDateTime>,
    pub created_at: OffsetDateTime,
}

macro_rules! to_comment {
    ($r:expr) => {
        Comment {
            id: $r.id,
            repo_id: RepoId::from_uuid($r.repo_id),
            subject_id: $r.subject_id,
            author_id: $r.author_id.map(UserId::from_uuid),
            author: $r.author,
            body: $r.body,
            edited_at: $r.edited_at,
            deleted_at: $r.deleted_at,
            hidden_at: $r.hidden_at,
            created_at: $r.created_at,
        }
    };
}

pub async fn insert_comment(
    exec: impl PgExecutor<'_>,
    repo: RepoId,
    kind: &str,
    subject: uuid::Uuid,
    author: UserId,
    body: &str,
) -> Result<uuid::Uuid, DbError> {
    let id = uuid::Uuid::now_v7();
    sqlx::query!(
        "INSERT INTO comments (id, repo_id, subject_kind, subject_id, author_id, body) VALUES ($1, $2, $3, $4, $5, $6)",
        id,
        repo.as_uuid(),
        kind,
        subject,
        author.as_uuid(),
        body
    )
    .execute(exec)
    .await?;
    Ok(id)
}

pub async fn comments(
    exec: impl PgExecutor<'_>,
    kind: &str,
    subject: uuid::Uuid,
) -> Result<Vec<Comment>, DbError> {
    let rows = sqlx::query!(
        r#"SELECT c.id, c.repo_id, c.subject_id, c.author_id, n.name::text AS author, c.body, c.edited_at,
                  c.deleted_at, c.hidden_at, c.created_at
           FROM comments c LEFT JOIN users u ON u.id = c.author_id AND u.state <> 'deleted'
           LEFT JOIN namespaces n ON n.id = u.namespace_id
           WHERE c.subject_kind = $1 AND c.subject_id = $2 ORDER BY c.created_at, c.id"#,
        kind,
        subject
    )
    .fetch_all(exec)
    .await?;
    Ok(rows.into_iter().map(|r| to_comment!(r)).collect())
}

pub async fn comment(
    exec: impl PgExecutor<'_>,
    id: uuid::Uuid,
) -> Result<Option<Comment>, DbError> {
    let r = sqlx::query!(
        r#"SELECT c.id, c.repo_id, c.subject_id, c.author_id, n.name::text AS author, c.body, c.edited_at,
                  c.deleted_at, c.hidden_at, c.created_at
           FROM comments c LEFT JOIN users u ON u.id = c.author_id AND u.state <> 'deleted'
           LEFT JOIN namespaces n ON n.id = u.namespace_id
           WHERE c.id = $1"#,
        id
    )
    .fetch_optional(exec)
    .await?;
    Ok(r.map(|r| to_comment!(r)))
}

pub async fn edit_comment(
    exec: impl PgExecutor<'_>,
    id: uuid::Uuid,
    body: &str,
) -> Result<bool, DbError> {
    let done = sqlx::query!(
        "UPDATE comments SET body = $2, edited_at = now() WHERE id = $1 AND deleted_at IS NULL",
        id,
        body
    )
    .execute(exec)
    .await?;
    Ok(done.rows_affected() == 1)
}

/// Soft-deletes a comment: its text is removed, the entry stays.
pub async fn delete_comment(exec: impl PgExecutor<'_>, id: uuid::Uuid) -> Result<bool, DbError> {
    let done = sqlx::query!(
        "UPDATE comments SET body = '', deleted_at = now() WHERE id = $1 AND deleted_at IS NULL",
        id
    )
    .execute(exec)
    .await?;
    Ok(done.rows_affected() == 1)
}

pub async fn set_comment_hidden(
    exec: impl PgExecutor<'_>,
    id: uuid::Uuid,
    hidden: bool,
) -> Result<bool, DbError> {
    let done = sqlx::query!(
        "UPDATE comments SET hidden_at = CASE WHEN $2 THEN now() ELSE NULL END
         WHERE id = $1 AND deleted_at IS NULL",
        id,
        hidden
    )
    .execute(exec)
    .await?;
    Ok(done.rows_affected() == 1)
}

#[derive(Debug, Clone)]
pub struct Event {
    pub id: uuid::Uuid,
    pub actor: Option<String>,
    pub kind: String,
    pub data: Value,
    pub created_at: OffsetDateTime,
}

pub async fn insert_event(
    exec: impl PgExecutor<'_>,
    repo: RepoId,
    kind_of_subject: &str,
    subject: uuid::Uuid,
    actor: Option<UserId>,
    kind: &str,
    data: &Value,
) -> Result<(), DbError> {
    sqlx::query!(
        "INSERT INTO timeline_events (id, repo_id, subject_kind, subject_id, actor_id, kind, data)
         VALUES ($1, $2, $3, $4, $5, $6, $7)",
        uuid::Uuid::now_v7(),
        repo.as_uuid(),
        kind_of_subject,
        subject,
        actor.map(|a| *a.as_uuid()),
        kind,
        data
    )
    .execute(exec)
    .await?;
    Ok(())
}

pub async fn events(
    exec: impl PgExecutor<'_>,
    kind: &str,
    subject: uuid::Uuid,
) -> Result<Vec<Event>, DbError> {
    let rows = sqlx::query!(
        r#"SELECT e.id, n.name::text AS actor, e.kind, e.data, e.created_at
           FROM timeline_events e LEFT JOIN users u ON u.id = e.actor_id AND u.state <> 'deleted'
           LEFT JOIN namespaces n ON n.id = u.namespace_id
           WHERE e.subject_kind = $1 AND e.subject_id = $2 ORDER BY e.created_at, e.id"#,
        kind,
        subject
    )
    .fetch_all(exec)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| Event {
            id: r.id,
            actor: r.actor,
            kind: r.kind,
            data: r.data,
            created_at: r.created_at,
        })
        .collect())
}
