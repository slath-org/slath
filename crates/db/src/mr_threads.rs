// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Review threads of merge requests, their comments, and viewed files.

use slath_core::ids::{MergeRequestId, UserId};
use slath_core::origin::Origin;
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
    /// A bot started it (findings, a bot's own threads); a deleted author
    /// counts as a person.
    pub author_bot: bool,
    pub resolved_at: Option<time::OffsetDateTime>,
    pub resolved_by: Option<String>,
    /// Who marked it addressed, and when (bots do; it isn't resolved).
    pub addressed_by_id: Option<UserId>,
    pub addressed_by: Option<String>,
    pub addressed_at: Option<time::OffsetDateTime>,
    pub created_at: time::OffsetDateTime,
    /// Set for a thread made from a status's annotation.
    pub finding: Option<Box<crate::mr_findings::Finding>>,
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
    /// Who wrote the body (origin labels).
    pub origin: Origin,
    pub pending: bool,
    /// A pending reply that resolves its thread when its review is
    /// submitted.
    pub resolves: bool,
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

/// Adds a comment; `resolves` (pending comments only) resolves its thread
/// when the review is submitted.
pub async fn insert_comment(
    exec: impl PgExecutor<'_>,
    thread: uuid::Uuid,
    author: UserId,
    body: &str,
    (pending, resolves): (bool, bool),
    origin: Origin,
) -> Result<uuid::Uuid, DbError> {
    let id = uuid::Uuid::now_v7();
    sqlx::query!(
        "INSERT INTO mr_thread_comments (id, thread_id, author_id, body, pending, resolves, origin)
         VALUES ($1, $2, $3, $4, $5, $6, $7)",
        id,
        thread,
        author.as_uuid(),
        body,
        pending,
        pending && resolves,
        origin.as_str()
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
                  t.line_hash, t.context, t.author_id, an.name::text AS author,
                  (au.kind = 'bot') IS TRUE AS "author_bot!", t.resolved_at,
                  rn.name::text AS resolved_by, t.addressed_by AS addressed_by_id,
                  dn.name::text AS addressed_by, t.addressed_at, t.created_at, f.context AS "finding_context?",
                  f.severity AS "finding_severity?", f.category AS "finding_category?"
           FROM mr_threads t
           LEFT JOIN mr_thread_findings f ON f.thread_id = t.id
           LEFT JOIN users au ON au.id = t.author_id AND au.state <> 'deleted'
           LEFT JOIN namespaces an ON an.id = au.namespace_id
           LEFT JOIN users ru ON ru.id = t.resolved_by AND ru.state <> 'deleted'
           LEFT JOIN namespaces rn ON rn.id = ru.namespace_id
           LEFT JOIN users du ON du.id = t.addressed_by AND du.state <> 'deleted'
           LEFT JOIN namespaces dn ON dn.id = du.namespace_id
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
            author_bot: r.author_bot,
            resolved_at: r.resolved_at,
            resolved_by: r.resolved_by,
            addressed_by_id: r.addressed_by_id.map(UserId::from_uuid),
            addressed_by: r.addressed_by,
            addressed_at: r.addressed_at,
            created_at: r.created_at,
            finding: match (r.finding_context, r.finding_severity, r.finding_category) {
                (Some(context), Some(severity), Some(category)) => {
                    Some(Box::new(crate::mr_findings::Finding {
                        context,
                        severity,
                        category,
                    }))
                }
                _ => None,
            },
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
                  (u.kind = 'bot') IS TRUE AS "author_bot!", c.body, c.origin, c.pending, c.resolves, c.created_at,
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
            origin: crate::origin(&r.origin),
            pending: r.pending,
            resolves: r.resolves,
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
) -> Result<Published, DbError> {
    // A reply by anyone but whoever marked a thread addressed clears it.
    let rows = sqlx::query!(
        r#"WITH published AS (
               UPDATE mr_thread_comments c SET pending = false, resolves = false, created_at = now()
               FROM mr_threads t, mr_thread_comments old
               WHERE t.id = c.thread_id AND t.mr_id = $1 AND c.author_id = $2 AND c.pending
                 AND old.id = c.id
               RETURNING c.thread_id, old.resolves),
           cleared AS (
               UPDATE mr_threads SET addressed_by = NULL, addressed_at = NULL
               WHERE id IN (SELECT thread_id FROM published) AND addressed_by IS DISTINCT FROM $2)
           SELECT thread_id AS "thread_id!", resolves AS "resolves!" FROM published"#,
        mr.as_uuid(),
        user.as_uuid()
    )
    .fetch_all(exec)
    .await?;
    let mut resolve: Vec<uuid::Uuid> = rows
        .iter()
        .filter(|r| r.resolves)
        .map(|r| r.thread_id)
        .collect();
    resolve.sort();
    resolve.dedup();
    Ok(Published {
        comments: u64::try_from(rows.len()).unwrap_or(0),
        resolve,
    })
}

/// What submitting a review published.
#[derive(Debug, Default)]
pub struct Published {
    pub comments: u64,
    /// Threads whose published replies resolve them.
    pub resolve: Vec<uuid::Uuid>,
}

/// Resolves (`Some(user)`) or reopens (`None`) a thread for a person; a
/// finding resolved this way isn't reopened by a later status.
/// Marks a thread addressed by `by`, or clears it (`None`); returns false
/// if nothing changed.
pub async fn set_addressed(
    exec: impl PgExecutor<'_>,
    thread: uuid::Uuid,
    by: Option<UserId>,
) -> Result<bool, DbError> {
    Ok(sqlx::query!(
        "UPDATE mr_threads SET addressed_by = $2,
             addressed_at = CASE WHEN $2::uuid IS NULL THEN NULL ELSE now() END
         WHERE id = $1 AND addressed_by IS DISTINCT FROM $2",
        thread,
        by.map(|u| *u.as_uuid())
    )
    .execute(exec)
    .await?
    .rows_affected()
        == 1)
}

/// Clears a thread's addressed mark after a reply by someone other than
/// whoever set it.
pub async fn clear_addressed_unless(
    exec: impl PgExecutor<'_>,
    thread: uuid::Uuid,
    author: UserId,
) -> Result<(), DbError> {
    sqlx::query!(
        "UPDATE mr_threads SET addressed_by = NULL, addressed_at = NULL
         WHERE id = $1 AND addressed_by IS NOT NULL AND addressed_by <> $2",
        thread,
        author.as_uuid()
    )
    .execute(exec)
    .await?;
    Ok(())
}

pub async fn set_resolved(
    exec: impl PgExecutor<'_>,
    thread: uuid::Uuid,
    by: Option<UserId>,
) -> Result<(), DbError> {
    sqlx::query!(
        "WITH f AS (UPDATE mr_thread_findings SET resolved_by_status = NULL WHERE thread_id = $1)
         UPDATE mr_threads SET resolved_at = CASE WHEN $2::uuid IS NULL THEN NULL ELSE now() END,
             resolved_by = $2, addressed_by = NULL, addressed_at = NULL WHERE id = $1",
        thread,
        by.map(|u| *u.as_uuid())
    )
    .execute(exec)
    .await?;
    Ok(())
}

/// Edits (with the editor's origin) or deletes a comment.
pub async fn edit_comment(
    exec: impl PgExecutor<'_>,
    comment: uuid::Uuid,
    body: Option<(&str, Origin)>,
) -> Result<(), DbError> {
    match body {
        Some((b, origin)) => {
            sqlx::query!(
                "UPDATE mr_thread_comments SET body = $2, edited_at = now(),
                     origin = slath_origin_least(origin, $3) WHERE id = $1",
                comment,
                b,
                origin.as_str()
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

/// The hunks a user marked viewed, as (path, key).
pub async fn viewed_hunks(
    exec: impl PgExecutor<'_>,
    mr: MergeRequestId,
    user: UserId,
) -> Result<Vec<(String, String)>, DbError> {
    let rows = sqlx::query!(
        "SELECT path, hunk_key FROM mr_viewed_hunks WHERE mr_id = $1 AND user_id = $2",
        mr.as_uuid(),
        user.as_uuid()
    )
    .fetch_all(exec)
    .await?;
    Ok(rows.into_iter().map(|r| (r.path, r.hunk_key)).collect())
}

/// How many hunks of `path` a user marked viewed.
pub async fn viewed_hunk_count(
    exec: impl PgExecutor<'_>,
    mr: MergeRequestId,
    user: UserId,
    path: &str,
) -> Result<i64, DbError> {
    Ok(sqlx::query_scalar!(
        r#"SELECT count(*) AS "n!" FROM mr_viewed_hunks
           WHERE mr_id = $1 AND user_id = $2 AND path = $3"#,
        mr.as_uuid(),
        user.as_uuid(),
        path
    )
    .fetch_one(exec)
    .await?)
}

/// Marks the hunks `keys` of `path` viewed, or not viewed.
pub async fn set_viewed_hunks(
    exec: impl PgExecutor<'_>,
    mr: MergeRequestId,
    user: UserId,
    path: &str,
    (keys, viewed): (&[String], bool),
) -> Result<(), DbError> {
    if viewed {
        sqlx::query!(
            "INSERT INTO mr_viewed_hunks (mr_id, user_id, path, hunk_key)
             SELECT $1, $2, $3, k FROM unnest($4::text[]) AS k
             ON CONFLICT DO NOTHING",
            mr.as_uuid(),
            user.as_uuid(),
            path,
            keys
        )
        .execute(exec)
        .await?;
    } else {
        sqlx::query!(
            "DELETE FROM mr_viewed_hunks
             WHERE mr_id = $1 AND user_id = $2 AND path = $3 AND hunk_key = ANY ($4)",
            mr.as_uuid(),
            user.as_uuid(),
            path,
            keys
        )
        .execute(exec)
        .await?;
    }
    Ok(())
}
