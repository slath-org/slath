// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Webhooks of repositories and organizations, their deliveries, and each
//! delivery's attempts.

use slath_core::ids::{OrgId, RepoId, UserId};
use sqlx::PgExecutor;
use time::OffsetDateTime;

use crate::DbError;

/// A webhook. The secret stays sealed here.
#[derive(Debug, Clone)]
pub struct Webhook {
    pub id: uuid::Uuid,
    pub repo_id: Option<RepoId>,
    pub org_id: Option<OrgId>,
    pub url: String,
    pub secret_enc: Vec<u8>,
    pub key_id: String,
    pub events: Vec<String>,
    pub active: bool,
    pub disabled_reason: Option<String>,
    pub consecutive_failures: i32,
    pub principal_id: Option<UserId>,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}

/// The owner of a webhook.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Owner {
    Repo(RepoId),
    Org(OrgId),
}

impl Owner {
    fn ids(self) -> (Option<uuid::Uuid>, Option<uuid::Uuid>) {
        match self {
            Self::Repo(r) => (Some(*r.as_uuid()), None),
            Self::Org(o) => (None, Some(*o.as_uuid())),
        }
    }
}

/// What a new webhook or an update stores.
#[derive(Debug, Clone)]
pub struct Fields<'a> {
    pub url: &'a str,
    pub events: &'a [String],
    pub active: bool,
    /// A new sealed secret and its key, if it changes.
    pub secret: Option<(&'a [u8], &'a str)>,
    pub principal: UserId,
}

macro_rules! webhook_from {
    ($r:expr) => {
        Webhook {
            id: $r.id,
            repo_id: $r.repo_id.map(RepoId::from_uuid),
            org_id: $r.org_id.map(OrgId::from_uuid),
            url: $r.url,
            secret_enc: $r.secret_enc,
            key_id: $r.key_id,
            events: $r.events,
            active: $r.active,
            disabled_reason: $r.disabled_reason,
            consecutive_failures: $r.consecutive_failures,
            principal_id: $r.principal_id.map(UserId::from_uuid),
            created_at: $r.created_at,
            updated_at: $r.updated_at,
        }
    };
}

/// Creates a webhook with ID `id` (the secret was sealed for it).
pub async fn insert(
    exec: impl PgExecutor<'_>,
    id: uuid::Uuid,
    owner: Owner,
    f: &Fields<'_>,
) -> Result<(), DbError> {
    let (repo, org) = owner.ids();
    let (secret, key) = f.secret.ok_or(DbError::Invalid("secret"))?;
    sqlx::query!(
        "INSERT INTO webhooks (id, repo_id, org_id, url, secret_enc, key_id, events, active, principal_id)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
        id,
        repo,
        org,
        f.url,
        secret,
        key,
        f.events,
        f.active,
        f.principal.as_uuid()
    )
    .execute(exec)
    .await?;
    Ok(())
}

/// Changes a webhook; a new secret replaces the old one. Turning it on
/// clears why it was off and its failure count.
pub async fn update(
    exec: impl PgExecutor<'_>,
    id: uuid::Uuid,
    f: &Fields<'_>,
) -> Result<(), DbError> {
    let (secret, key) = f.secret.map_or((None, None), |(s, k)| (Some(s), Some(k)));
    sqlx::query!(
        "UPDATE webhooks SET url = $2, events = $3, active = $4,
                secret_enc = COALESCE($5, secret_enc), key_id = COALESCE($6, key_id),
                principal_id = $7,
                disabled_reason = CASE WHEN $4 THEN NULL ELSE disabled_reason END,
                consecutive_failures = CASE WHEN $4 AND NOT active THEN 0 ELSE consecutive_failures END,
                updated_at = now()
          WHERE id = $1",
        id,
        f.url,
        f.events,
        f.active,
        secret,
        key,
        f.principal.as_uuid()
    )
    .execute(exec)
    .await?;
    Ok(())
}

pub async fn delete(exec: impl PgExecutor<'_>, id: uuid::Uuid) -> Result<(), DbError> {
    sqlx::query!("DELETE FROM webhooks WHERE id = $1", id)
        .execute(exec)
        .await?;
    Ok(())
}

pub async fn get(exec: impl PgExecutor<'_>, id: uuid::Uuid) -> Result<Option<Webhook>, DbError> {
    let r = sqlx::query!("SELECT * FROM webhooks WHERE id = $1", id)
        .fetch_optional(exec)
        .await?;
    Ok(r.map(|r| webhook_from!(r)))
}

/// The webhooks of a repository or organization, oldest first.
pub async fn list(exec: impl PgExecutor<'_>, owner: Owner) -> Result<Vec<Webhook>, DbError> {
    let (repo, org) = owner.ids();
    let rows = sqlx::query!(
        "SELECT * FROM webhooks
          WHERE ($1::uuid IS NOT NULL AND repo_id = $1) OR ($2::uuid IS NOT NULL AND org_id = $2)
          ORDER BY created_at, id",
        repo,
        org
    )
    .fetch_all(exec)
    .await?;
    Ok(rows.into_iter().map(|r| webhook_from!(r)).collect())
}

/// The active webhooks that hear about `repo`: its own, and its
/// organization's.
pub async fn for_repo(exec: impl PgExecutor<'_>, repo: RepoId) -> Result<Vec<Webhook>, DbError> {
    let rows = sqlx::query!(
        "SELECT w.* FROM webhooks w
          WHERE w.active AND (w.repo_id = $1 OR w.org_id = (
                SELECT o.id FROM repositories r JOIN organizations o ON o.namespace_id = r.namespace_id
                 WHERE r.id = $1))
          ORDER BY w.created_at, w.id",
        repo.as_uuid()
    )
    .fetch_all(exec)
    .await?;
    Ok(rows.into_iter().map(|r| webhook_from!(r)).collect())
}

/// Switches a webhook off, saying why.
pub async fn disable(
    exec: impl PgExecutor<'_>,
    id: uuid::Uuid,
    reason: &str,
) -> Result<bool, DbError> {
    Ok(sqlx::query!(
        "UPDATE webhooks SET active = false, disabled_reason = $2, updated_at = now()
          WHERE id = $1 AND active",
        id,
        reason
    )
    .execute(exec)
    .await?
    .rows_affected()
        > 0)
}

/// Counts a failed attempt; returns the new count of failures in a row.
pub async fn add_failure(exec: impl PgExecutor<'_>, id: uuid::Uuid) -> Result<i32, DbError> {
    Ok(sqlx::query_scalar!(
        "UPDATE webhooks SET consecutive_failures = consecutive_failures + 1 WHERE id = $1
         RETURNING consecutive_failures",
        id
    )
    .fetch_optional(exec)
    .await?
    .unwrap_or(0))
}

pub async fn reset_failures(exec: impl PgExecutor<'_>, id: uuid::Uuid) -> Result<(), DbError> {
    sqlx::query!(
        "UPDATE webhooks SET consecutive_failures = 0 WHERE id = $1 AND consecutive_failures <> 0",
        id
    )
    .execute(exec)
    .await?;
    Ok(())
}

/// A delivery: one event for one webhook, with its retries.
#[derive(Debug, Clone)]
pub struct Delivery {
    pub id: uuid::Uuid,
    pub webhook_id: uuid::Uuid,
    pub event: String,
    pub action: Option<String>,
    pub payload: serde_json::Value,
    /// `pending`, `delivered`, or `failed`.
    pub state: String,
    pub attempts: i32,
    pub next_attempt_at: Option<OffsetDateTime>,
    pub redelivery_of: Option<uuid::Uuid>,
    pub created_at: OffsetDateTime,
    pub finished_at: Option<OffsetDateTime>,
    /// The last attempt's HTTP status, if it got one.
    pub last_status: Option<i32>,
}

pub async fn insert_delivery(
    exec: impl PgExecutor<'_>,
    id: uuid::Uuid,
    webhook: uuid::Uuid,
    (event, action): (&str, Option<&str>),
    payload: &serde_json::Value,
    redelivery_of: Option<uuid::Uuid>,
) -> Result<(), DbError> {
    sqlx::query!(
        "INSERT INTO webhook_deliveries (id, webhook_id, event, action, payload, redelivery_of)
         VALUES ($1, $2, $3, $4, $5, $6)",
        id,
        webhook,
        event,
        action,
        payload,
        redelivery_of
    )
    .execute(exec)
    .await?;
    Ok(())
}

pub async fn delivery(
    exec: impl PgExecutor<'_>,
    id: uuid::Uuid,
) -> Result<Option<Delivery>, DbError> {
    let r = sqlx::query!(
        r#"SELECT d.*, (SELECT a.status FROM webhook_attempts a WHERE a.delivery_id = d.id
                         ORDER BY a.attempt DESC LIMIT 1) AS last_status
             FROM webhook_deliveries d WHERE d.id = $1"#,
        id
    )
    .fetch_optional(exec)
    .await?;
    Ok(r.map(|r| Delivery {
        id: r.id,
        webhook_id: r.webhook_id,
        event: r.event,
        action: r.action,
        payload: r.payload,
        state: r.state,
        attempts: r.attempts,
        next_attempt_at: r.next_attempt_at,
        redelivery_of: r.redelivery_of,
        created_at: r.created_at,
        finished_at: r.finished_at,
        last_status: r.last_status,
    }))
}

/// A webhook's deliveries, newest first.
pub async fn deliveries(
    exec: impl PgExecutor<'_>,
    webhook: uuid::Uuid,
    limit: i64,
) -> Result<Vec<Delivery>, DbError> {
    let rows = sqlx::query!(
        r#"SELECT d.*, (SELECT a.status FROM webhook_attempts a WHERE a.delivery_id = d.id
                         ORDER BY a.attempt DESC LIMIT 1) AS last_status
             FROM webhook_deliveries d WHERE d.webhook_id = $1
            ORDER BY d.created_at DESC, d.id DESC LIMIT $2"#,
        webhook,
        limit
    )
    .fetch_all(exec)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| Delivery {
            id: r.id,
            webhook_id: r.webhook_id,
            event: r.event,
            action: r.action,
            payload: r.payload,
            state: r.state,
            attempts: r.attempts,
            next_attempt_at: r.next_attempt_at,
            redelivery_of: r.redelivery_of,
            created_at: r.created_at,
            finished_at: r.finished_at,
            last_status: r.last_status,
        })
        .collect())
}

/// One attempt of a delivery.
#[derive(Debug, Clone)]
pub struct Attempt {
    pub attempt: i32,
    pub started_at: OffsetDateTime,
    pub duration_ms: i32,
    pub status: Option<i32>,
    pub response: Option<String>,
    pub error: Option<String>,
}

pub async fn attempts(
    exec: impl PgExecutor<'_>,
    delivery: uuid::Uuid,
) -> Result<Vec<Attempt>, DbError> {
    Ok(sqlx::query_as!(
        Attempt,
        "SELECT attempt, started_at, duration_ms, status, response, error
           FROM webhook_attempts WHERE delivery_id = $1 ORDER BY attempt",
        delivery
    )
    .fetch_all(exec)
    .await?)
}

/// Records an attempt and what it means for the delivery: `state` and,
/// while pending, when to try again.
pub async fn record_attempt(
    exec: impl PgExecutor<'_> + Copy,
    delivery: uuid::Uuid,
    a: &Attempt,
    (state, next): (&str, Option<OffsetDateTime>),
) -> Result<(), DbError> {
    sqlx::query!(
        "INSERT INTO webhook_attempts (delivery_id, attempt, started_at, duration_ms, status, response, error)
         VALUES ($1, $2, $3, $4, $5, $6, $7)",
        delivery,
        a.attempt,
        a.started_at,
        a.duration_ms,
        a.status,
        a.response,
        a.error
    )
    .execute(exec)
    .await?;
    sqlx::query!(
        "UPDATE webhook_deliveries SET attempts = $2, state = $3, next_attempt_at = $4,
                finished_at = CASE WHEN $3 = 'pending' THEN NULL ELSE now() END
          WHERE id = $1",
        delivery,
        a.attempt,
        state,
        next
    )
    .execute(exec)
    .await?;
    Ok(())
}

/// Ends a pending delivery without another attempt (its webhook is off
/// or gone).
pub async fn give_up(exec: impl PgExecutor<'_>, delivery: uuid::Uuid) -> Result<(), DbError> {
    sqlx::query!(
        "UPDATE webhook_deliveries SET state = 'failed', next_attempt_at = NULL, finished_at = now()
          WHERE id = $1 AND state = 'pending'",
        delivery
    )
    .execute(exec)
    .await?;
    Ok(())
}

/// Deletes deliveries older than `days` days.
pub async fn purge(exec: impl PgExecutor<'_>, days: i32) -> Result<u64, DbError> {
    Ok(sqlx::query!(
        "DELETE FROM webhook_deliveries WHERE created_at < now() - make_interval(days => $1)",
        days
    )
    .execute(exec)
    .await?
    .rows_affected())
}

/// Webhooks whose principal is `user`: switched off when that account is
/// deleted or suspended.
pub async fn of_principal(
    exec: impl PgExecutor<'_>,
    user: UserId,
) -> Result<Vec<uuid::Uuid>, DbError> {
    Ok(sqlx::query_scalar!(
        "SELECT id FROM webhooks WHERE principal_id = $1 AND active",
        user.as_uuid()
    )
    .fetch_all(exec)
    .await?)
}
