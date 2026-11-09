// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Review threads of merge requests, their comments, and viewed files.

use slath_core::ids::{MergeRequestId, UserId};
use sqlx::PgExecutor;

use crate::DbError;

/// Where a thread is anchored; `None` fields for merge-request-wide threads.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Anchor {
    pub old_oid: Option<String>,
    pub new_oid: Option<String>,
    pub path: Option<String>,
    /// `old` or `new`.
    pub side: Option<String>,
    pub line_start: Option<i32>,
    pub line_end: Option<i32>,
    pub line_hash: Option<String>,
    pub context: serde_json::Value,
}

/// A thread.
#[derive(Debug, Clone, PartialEq)]
pub struct Thread {
    pub id: uuid::Uuid,
    pub revision_seq: i32,
    pub anchor: Anchor,
    pub author_id: Option<UserId>,
    pub author: Option<String>,
    pub resolved_at: Option<time::OffsetDateTime>,
    pub resolved_by: Option<String>,
    pub created_at: time::OffsetDateTime,
}

/// A comment of a thread.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThreadComment {
    pub id: uuid::Uuid,
    pub thread_id: uuid::Uuid,
    pub author_id: Option<UserId>,
    pub author: Option<String>,
    pub author_bot: bool,
    pub body: String,
    pub pending: bool,
    pub created_at: time::OffsetDateTime,
    pub edited_at: Option<time::OffsetDateTime>,
    pub deleted_at: Option<time::OffsetDateTime>,
}

pub async fn insert_thread(
    exec: impl PgExecutor<'_>,
    mr: MergeRequestId,
    revision_seq: i32,
    a: &Anchor,
    author: UserId,
) -> Result<uuid::Uuid, DbError> {
    let id = uuid::Uuid::now_v7();
    sqlx::query!(
        "INSERT INTO mr_threads (id, mr_id, revision_seq, old_oid, new_oid, path, side, line_start, line_end,
             line_hash, context, author_id)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)",
        id,
        mr.as_uuid(),
        revision_seq,
        a.old_oid,
        a.new_oid,
        a.path,
        a.side,
        a.line_start,
        a.line_end,
        a.line_hash,
        if a.context.is_null() { serde_json::json!({}) } else { a.context.clone() },
        author.as_uuid()
    )
    .execute(exec)
    .await?;
    Ok(id)
}

pub async fn insert_comment(
    exec: impl PgExecutor<'_>,
    thread: uuid::Uuid,
    author: UserId,
    body: &str,
    pending: bool,
) -> Result<uuid::Uuid, DbError> {
    let id = uuid::Uuid::now_v7();
    sqlx::query!(
        "INSERT INTO mr_thread_comments (id, thread_id, author_id, body, pending) VALUES ($1, $2, $3, $4, $5)",
        id,
        thread,
        author.as_uuid(),
        body,
        pending
    )
    .execute(exec)
    .await?;
    Ok(id)
}

/// Every thread of a merge request, oldest first.
pub async fn threads(
    exec: impl PgExecutor<'_>,
    mr: MergeRequestId,
) -> Result<Vec<Thread>, DbError> {
    let rows = sqlx::query!(
        r#"SELECT t.id, t.revision_seq, t.old_oid, t.new_oid, t.path, t.side, t.line_start, t.line_end,
                  t.line_hash, t.context, t.author_id, an.name::text AS author, t.resolved_at,
                  rn.name::text AS resolved_by, t.created_at
           FROM mr_threads t
           LEFT JOIN users au ON au.id = t.author_id AND au.state <> 'deleted'
           LEFT JOIN namespaces an ON an.id = au.namespace_id
           LEFT JOIN users ru ON ru.id = t.resolved_by AND ru.state <> 'deleted'
           LEFT JOIN namespaces rn ON rn.id = ru.namespace_id
           WHERE t.mr_id = $1 ORDER BY t.created_at, t.id"#,
        mr.as_uuid()
    )
    .fetch_all(exec)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| Thread {
            id: r.id,
            revision_seq: r.revision_seq,
            anchor: Anchor {
                old_oid: r.old_oid,
                new_oid: r.new_oid,
                path: r.path,
                side: r.side,
                line_start: r.line_start,
                line_end: r.line_end,
                line_hash: r.line_hash,
                context: r.context,
            },
            author_id: r.author_id.map(UserId::from_uuid),
            author: r.author,
            resolved_at: r.resolved_at,
            resolved_by: r.resolved_by,
            created_at: r.created_at,
        })
        .collect())
}

/// Every comment of the merge request's threads, oldest first.
pub async fn comments(
    exec: impl PgExecutor<'_>,
    mr: MergeRequestId,
) -> Result<Vec<ThreadComment>, DbError> {
    let rows = sqlx::query!(
        r#"SELECT c.id, c.thread_id, c.author_id, n.name::text AS author,
                  (u.kind = 'bot') IS TRUE AS "author_bot!", c.body, c.pending, c.created_at,
                  c.edited_at, c.deleted_at
           FROM mr_thread_comments c JOIN mr_threads t ON t.id = c.thread_id
           LEFT JOIN users u ON u.id = c.author_id AND u.state <> 'deleted'
           LEFT JOIN namespaces n ON n.id = u.namespace_id
           WHERE t.mr_id = $1 ORDER BY c.created_at, c.id"#,
        mr.as_uuid()
    )
    .fetch_all(exec)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| ThreadComment {
            id: r.id,
            thread_id: r.thread_id,
            author_id: r.author_id.map(UserId::from_uuid),
            author: r.author,
            author_bot: r.author_bot,
            body: r.body,
            pending: r.pending,
            created_at: r.created_at,
            edited_at: r.edited_at,
            deleted_at: r.deleted_at,
        })
        .collect())
}

/// Publishes a user's pending comments on a merge request (submitting
/// their review); returns how many.
pub async fn publish_pending(
    exec: impl PgExecutor<'_>,
    mr: MergeRequestId,
    user: UserId,
) -> Result<u64, DbError> {
    let done = sqlx::query!(
        "UPDATE mr_thread_comments c SET pending = false, created_at = now()
         FROM mr_threads t
         WHERE t.id = c.thread_id AND t.mr_id = $1 AND c.author_id = $2 AND c.pending",
        mr.as_uuid(),
        user.as_uuid()
    )
    .execute(exec)
    .await?;
    Ok(done.rows_affected())
}

/// Resolves (`Some(user)`) or reopens (`None`) a thread.
pub async fn set_resolved(
    exec: impl PgExecutor<'_>,
    thread: uuid::Uuid,
    by: Option<UserId>,
) -> Result<(), DbError> {
    sqlx::query!(
        "UPDATE mr_threads SET resolved_at = CASE WHEN $2::uuid IS NULL THEN NULL ELSE now() END,
             resolved_by = $2 WHERE id = $1",
        thread,
        by.map(|u| *u.as_uuid())
    )
    .execute(exec)
    .await?;
    Ok(())
}

/// Edits or deletes a comment.
pub async fn edit_comment(
    exec: impl PgExecutor<'_>,
    comment: uuid::Uuid,
    body: Option<&str>,
) -> Result<(), DbError> {
    match body {
        Some(b) => {
            sqlx::query!(
                "UPDATE mr_thread_comments SET body = $2, edited_at = now() WHERE id = $1",
                comment,
                b
            )
            .execute(exec)
            .await?;
        }
        None => {
            sqlx::query!(
                "UPDATE mr_thread_comments SET deleted_at = now() WHERE id = $1",
                comment
            )
            .execute(exec)
            .await?;
        }
    }
    Ok(())
}

/// The files a user marked viewed, with the blob they saw.
pub async fn viewed(
    exec: impl PgExecutor<'_>,
    mr: MergeRequestId,
    user: UserId,
) -> Result<Vec<(String, String)>, DbError> {
    let rows = sqlx::query!(
        "SELECT path, blob_oid FROM mr_viewed_files WHERE mr_id = $1 AND user_id = $2",
        mr.as_uuid(),
        user.as_uuid()
    )
    .fetch_all(exec)
    .await?;
    Ok(rows.into_iter().map(|r| (r.path, r.blob_oid)).collect())
}

/// Marks a file viewed at `blob`, or not viewed (`None`).
pub async fn set_viewed(
    exec: impl PgExecutor<'_>,
    mr: MergeRequestId,
    user: UserId,
    path: &str,
    blob: Option<&str>,
) -> Result<(), DbError> {
    match blob {
        Some(b) => {
            sqlx::query!(
                "INSERT INTO mr_viewed_files (mr_id, user_id, path, blob_oid) VALUES ($1, $2, $3, $4)
                 ON CONFLICT (mr_id, user_id, path) DO UPDATE SET blob_oid = EXCLUDED.blob_oid",
                mr.as_uuid(),
                user.as_uuid(),
                path,
                b
            )
            .execute(exec)
            .await?;
        }
        None => {
            sqlx::query!(
                "DELETE FROM mr_viewed_files WHERE mr_id = $1 AND user_id = $2 AND path = $3",
                mr.as_uuid(),
                user.as_uuid(),
                path
            )
            .execute(exec)
            .await?;
        }
    }
    Ok(())
}
