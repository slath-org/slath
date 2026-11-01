// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Subscriptions and the notification inbox.

use slath_core::ids::{RepoId, UserId};
use sqlx::PgExecutor;
use time::OffsetDateTime;

use crate::DbError;

/// One inbox row.
#[derive(Debug, Clone)]
pub struct Notification {
    pub id: uuid::Uuid,
    pub repo_id: RepoId,
    pub subject_kind: String,
    pub subject_id: uuid::Uuid,
    pub reason: String,
    pub unread: bool,
    pub updated_at: OffsetDateTime,
}

/// What a user set for a repository or thread.
pub async fn subscription(
    exec: impl PgExecutor<'_>,
    user: UserId,
    kind: &str,
    subject: uuid::Uuid,
) -> Result<Option<String>, DbError> {
    Ok(sqlx::query_scalar!(
        "SELECT state FROM subscriptions WHERE user_id = $1 AND subject_kind = $2 AND subject_id = $3",
        user.as_uuid(),
        kind,
        subject
    )
    .fetch_optional(exec)
    .await?)
}

/// Sets (`Some`) or clears (`None`) a subscription.
pub async fn set_subscription(
    exec: impl PgExecutor<'_>,
    user: UserId,
    kind: &str,
    subject: uuid::Uuid,
    repo: RepoId,
    state: Option<&str>,
) -> Result<(), DbError> {
    match state {
        Some(state) => {
            sqlx::query!(
                "INSERT INTO subscriptions (user_id, subject_kind, subject_id, repo_id, state)
                 VALUES ($1, $2, $3, $4, $5)
                 ON CONFLICT (user_id, subject_kind, subject_id) DO UPDATE SET state = EXCLUDED.state",
                user.as_uuid(),
                kind,
                subject,
                repo.as_uuid(),
                state
            )
            .execute(exec)
            .await?;
        }
        None => {
            sqlx::query!(
                "DELETE FROM subscriptions WHERE user_id = $1 AND subject_kind = $2 AND subject_id = $3",
                user.as_uuid(),
                kind,
                subject
            )
            .execute(exec)
            .await?;
        }
    }
    Ok(())
}

/// Users with a subscription in `state` to a subject.
pub async fn subscribers(
    exec: impl PgExecutor<'_>,
    kind: &str,
    subject: uuid::Uuid,
    state: &str,
) -> Result<Vec<UserId>, DbError> {
    Ok(sqlx::query_scalar!(
        "SELECT user_id FROM subscriptions WHERE subject_kind = $1 AND subject_id = $2 AND state = $3",
        kind,
        subject,
        state
    )
    .fetch_all(exec)
    .await?
    .into_iter()
    .map(UserId::from_uuid)
    .collect())
}

/// Authors of an issue's comments (not deleted).
pub async fn commenters(
    exec: impl PgExecutor<'_>,
    issue: uuid::Uuid,
) -> Result<Vec<UserId>, DbError> {
    Ok(sqlx::query_scalar!(
        r#"SELECT DISTINCT author_id AS "id!" FROM comments
            WHERE subject_kind = 'issue' AND subject_id = $1 AND author_id IS NOT NULL
              AND deleted_at IS NULL"#,
        issue
    )
    .fetch_all(exec)
    .await?
    .into_iter()
    .map(UserId::from_uuid)
    .collect())
}

/// What the fan-out needs about a recipient.
#[derive(Debug, Clone)]
pub struct Recipient {
    pub name: String,
    pub email: Option<String>,
    pub email_verified: bool,
    pub email_notifications: bool,
}

pub async fn recipient(
    exec: impl PgExecutor<'_>,
    user: UserId,
) -> Result<Option<Recipient>, DbError> {
    let r = sqlx::query!(
        r#"SELECT n.name::text AS "name!", u.email::text AS email, u.email_verified,
                  u.email_notifications
             FROM users u JOIN namespaces n ON n.id = u.namespace_id
            WHERE u.id = $1 AND u.state = 'active'"#,
        user.as_uuid()
    )
    .fetch_optional(exec)
    .await?;
    Ok(r.map(|r| Recipient {
        name: r.name,
        email: r.email,
        email_verified: r.email_verified,
        email_notifications: r.email_notifications,
    }))
}

/// Puts a thread at the top of a user's inbox, unread.
pub async fn bump(
    exec: impl PgExecutor<'_>,
    user: UserId,
    repo: RepoId,
    kind: &str,
    subject: uuid::Uuid,
    reason: &str,
) -> Result<(), DbError> {
    sqlx::query!(
        "INSERT INTO notifications (id, user_id, repo_id, subject_kind, subject_id, reason)
         VALUES ($1, $2, $3, $4, $5, $6)
         ON CONFLICT (user_id, subject_kind, subject_id)
         DO UPDATE SET unread = true, updated_at = now(), reason = EXCLUDED.reason",
        uuid::Uuid::now_v7(),
        user.as_uuid(),
        repo.as_uuid(),
        kind,
        subject,
        reason
    )
    .execute(exec)
    .await?;
    Ok(())
}

/// A page of a user's inbox, newest first.
pub async fn inbox(
    exec: impl PgExecutor<'_>,
    user: UserId,
    unread_only: bool,
    offset: i64,
    limit: i64,
) -> Result<Vec<Notification>, DbError> {
    let rows = sqlx::query!(
        "SELECT id, repo_id, subject_kind, subject_id, reason, unread, updated_at
           FROM notifications WHERE user_id = $1 AND (unread OR NOT $2)
          ORDER BY updated_at DESC, id DESC OFFSET $3 LIMIT $4",
        user.as_uuid(),
        unread_only,
        offset,
        limit
    )
    .fetch_all(exec)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| Notification {
            id: r.id,
            repo_id: RepoId::from_uuid(r.repo_id),
            subject_kind: r.subject_kind,
            subject_id: r.subject_id,
            reason: r.reason,
            unread: r.unread,
            updated_at: r.updated_at,
        })
        .collect())
}

/// Marks one of the user's notifications read or unread.
pub async fn set_unread(
    exec: impl PgExecutor<'_>,
    user: UserId,
    id: uuid::Uuid,
    unread: bool,
) -> Result<bool, DbError> {
    Ok(sqlx::query!(
        "UPDATE notifications SET unread = $3 WHERE id = $2 AND user_id = $1",
        user.as_uuid(),
        id,
        unread
    )
    .execute(exec)
    .await?
    .rows_affected()
        == 1)
}

/// Marks the user's notification about a thread read (when they open it).
pub async fn mark_thread_read(
    exec: impl PgExecutor<'_>,
    user: UserId,
    kind: &str,
    subject: uuid::Uuid,
) -> Result<(), DbError> {
    sqlx::query!(
        "UPDATE notifications SET unread = false
          WHERE user_id = $1 AND subject_kind = $2 AND subject_id = $3 AND unread",
        user.as_uuid(),
        kind,
        subject
    )
    .execute(exec)
    .await?;
    Ok(())
}

pub async fn mark_all_read(exec: impl PgExecutor<'_>, user: UserId) -> Result<(), DbError> {
    sqlx::query!(
        "UPDATE notifications SET unread = false WHERE user_id = $1 AND unread",
        user.as_uuid()
    )
    .execute(exec)
    .await?;
    Ok(())
}

pub async fn set_email_notifications(
    exec: impl PgExecutor<'_>,
    user: UserId,
    on: bool,
) -> Result<(), DbError> {
    sqlx::query!(
        "UPDATE users SET email_notifications = $2, updated_at = now() WHERE id = $1",
        user.as_uuid(),
        on
    )
    .execute(exec)
    .await?;
    Ok(())
}

pub async fn email_notifications(exec: impl PgExecutor<'_>, user: UserId) -> Result<bool, DbError> {
    Ok(sqlx::query_scalar!(
        "SELECT email_notifications FROM users WHERE id = $1",
        user.as_uuid()
    )
    .fetch_one(exec)
    .await?)
}

/// An issue by ID, as `(repo, number)`.
pub async fn issue_number(
    exec: impl PgExecutor<'_>,
    issue: uuid::Uuid,
) -> Result<Option<(RepoId, i32)>, DbError> {
    let r = sqlx::query!("SELECT repo_id, number FROM issues WHERE id = $1", issue)
        .fetch_optional(exec)
        .await?;
    Ok(r.map(|r| (RepoId::from_uuid(r.repo_id), r.number)))
}
