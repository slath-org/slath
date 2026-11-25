// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Work orders: an issue or a merge request delegated to a bot, and the
//! events that record what was decided under them.

use slath_core::ids::{RepoId, UserId};
use sqlx::PgExecutor;
use time::OffsetDateTime;

use crate::DbError;

/// A work order.
#[derive(Debug, Clone)]
pub struct WorkOrder {
    pub id: uuid::Uuid,
    pub repo_id: RepoId,
    pub number: i32,
    /// `issue` or `merge_request`.
    pub target_kind: String,
    pub issue_id: Option<uuid::Uuid>,
    pub mr_id: Option<uuid::Uuid>,
    pub bot_id: UserId,
    pub creator_id: Option<UserId>,
    pub origin: String,
    pub goal: String,
    pub instructions: Option<String>,
    pub scope: Vec<String>,
    pub max_revisions: i32,
    pub max_open_mrs: i32,
    pub expires_at: OffsetDateTime,
    pub required_checks: Vec<String>,
    pub reviewer_ids: Vec<uuid::Uuid>,
    /// `open` or `ended`.
    pub state: String,
    pub end_reason: Option<String>,
    pub ended_by: Option<UserId>,
    pub ended_at: Option<OffsetDateTime>,
    pub claimed_at: Option<OffsetDateTime>,
    pub created_at: OffsetDateTime,
}

/// A new order; the caller has checked every field.
#[derive(Debug, Clone)]
pub struct NewWorkOrder<'a> {
    pub repo_id: RepoId,
    pub target_kind: &'a str,
    pub issue_id: Option<uuid::Uuid>,
    pub mr_id: Option<uuid::Uuid>,
    pub bot_id: UserId,
    pub creator_id: UserId,
    pub origin: &'a str,
    pub goal: &'a str,
    pub instructions: Option<&'a str>,
    pub scope: &'a [String],
    pub max_revisions: i32,
    pub max_open_mrs: i32,
    pub expires_at: OffsetDateTime,
    pub required_checks: &'a [String],
    pub reviewer_ids: &'a [uuid::Uuid],
}

/// One decision under an order.
#[derive(Debug, Clone)]
pub struct Event {
    pub id: uuid::Uuid,
    pub action: String,
    /// `allowed` or `denied`.
    pub decision: String,
    pub policy: Option<String>,
    pub target: Option<String>,
    pub actor_id: Option<UserId>,
    pub created_at: OffsetDateTime,
}

macro_rules! order {
    ($r:expr) => {{
        let r = $r;
        WorkOrder {
            id: r.id,
            repo_id: RepoId::from_uuid(r.repo_id),
            number: r.number,
            target_kind: r.target_kind,
            issue_id: r.issue_id,
            mr_id: r.mr_id,
            bot_id: UserId::from_uuid(r.bot_id),
            creator_id: r.creator_id.map(UserId::from_uuid),
            origin: r.origin,
            goal: r.goal,
            instructions: r.instructions,
            scope: r.scope,
            max_revisions: r.max_revisions,
            max_open_mrs: r.max_open_mrs,
            expires_at: r.expires_at,
            required_checks: r.required_checks,
            reviewer_ids: r.reviewer_ids,
            state: r.state,
            end_reason: r.end_reason,
            ended_by: r.ended_by.map(UserId::from_uuid),
            ended_at: r.ended_at,
            claimed_at: r.claimed_at,
            created_at: r.created_at,
        }
    }};
}

/// Inserts an order with the repository's next order number. Fails with a
/// unique violation (`work_orders_open_issue`, `work_orders_open_mr`) if
/// the target has an open order.
pub async fn insert(
    tx: &mut sqlx::PgConnection,
    o: &NewWorkOrder<'_>,
) -> Result<WorkOrder, DbError> {
    let number = sqlx::query_scalar!(
        "UPDATE repositories SET next_work_order_number = next_work_order_number + 1 WHERE id = $1
         RETURNING next_work_order_number - 1 AS \"n!\"",
        o.repo_id.as_uuid()
    )
    .fetch_one(&mut *tx)
    .await?;
    let r = sqlx::query!(
        "INSERT INTO work_orders (id, repo_id, number, target_kind, issue_id, mr_id, bot_id,
             creator_id, origin, goal, instructions, scope, max_revisions, max_open_mrs, expires_at,
             required_checks, reviewer_ids)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17)
         RETURNING *",
        uuid::Uuid::now_v7(),
        o.repo_id.as_uuid(),
        number,
        o.target_kind,
        o.issue_id,
        o.mr_id,
        o.bot_id.as_uuid(),
        o.creator_id.as_uuid(),
        o.origin,
        o.goal,
        o.instructions,
        o.scope,
        o.max_revisions,
        o.max_open_mrs,
        o.expires_at,
        o.required_checks,
        o.reviewer_ids
    )
    .fetch_one(&mut *tx)
    .await;
    match r {
        Ok(r) => Ok(order!(r)),
        Err(sqlx::Error::Database(e))
            if matches!(
                e.constraint(),
                Some("work_orders_open_issue" | "work_orders_open_mr")
            ) =>
        {
            Err(DbError::Conflict("target"))
        }
        Err(e) => Err(e.into()),
    }
}

/// An order by its number in a repository.
pub async fn get(
    exec: impl PgExecutor<'_>,
    repo: RepoId,
    number: i32,
) -> Result<Option<WorkOrder>, DbError> {
    Ok(sqlx::query!(
        "SELECT * FROM work_orders WHERE repo_id = $1 AND number = $2",
        repo.as_uuid(),
        number
    )
    .fetch_optional(exec)
    .await?
    .map(|r| order!(r)))
}

/// An order by ID.
pub async fn get_by_id(
    exec: impl PgExecutor<'_>,
    id: uuid::Uuid,
) -> Result<Option<WorkOrder>, DbError> {
    Ok(sqlx::query!("SELECT * FROM work_orders WHERE id = $1", id)
        .fetch_optional(exec)
        .await?
        .map(|r| order!(r)))
}

/// A repository's orders, newest first; with `state`, only those.
pub async fn list(
    exec: impl PgExecutor<'_>,
    repo: RepoId,
    state: Option<&str>,
    offset: i64,
    limit: i64,
) -> Result<Vec<WorkOrder>, DbError> {
    Ok(sqlx::query!(
        "SELECT * FROM work_orders WHERE repo_id = $1 AND ($2::text IS NULL OR state = $2)
         ORDER BY number DESC OFFSET $3 LIMIT $4",
        repo.as_uuid(),
        state,
        offset,
        limit
    )
    .fetch_all(exec)
    .await?
    .into_iter()
    .map(|r| order!(r))
    .collect())
}

/// A bot's open orders, oldest first; with `repos`, only in those.
pub async fn open_for_bot(
    exec: impl PgExecutor<'_>,
    bot: UserId,
    repos: Option<&[uuid::Uuid]>,
) -> Result<Vec<WorkOrder>, DbError> {
    Ok(sqlx::query!(
        "SELECT * FROM work_orders WHERE bot_id = $1 AND state = 'open'
           AND ($2::uuid[] IS NULL OR repo_id = ANY($2))
         ORDER BY created_at, id",
        bot.as_uuid(),
        repos
    )
    .fetch_all(exec)
    .await?
    .into_iter()
    .map(|r| order!(r))
    .collect())
}

/// The open order of an issue or merge request, if any.
pub async fn open_for_target(
    exec: impl PgExecutor<'_>,
    issue: Option<uuid::Uuid>,
    mr: Option<uuid::Uuid>,
) -> Result<Option<WorkOrder>, DbError> {
    Ok(sqlx::query!(
        "SELECT * FROM work_orders WHERE state = 'open'
           AND (issue_id = $1 OR mr_id = $2)",
        issue,
        mr
    )
    .fetch_optional(exec)
    .await?
    .map(|r| order!(r)))
}

/// The newest order of an issue or merge request, open or ended.
pub async fn latest_for_target(
    exec: impl PgExecutor<'_>,
    issue: Option<uuid::Uuid>,
    mr: Option<uuid::Uuid>,
) -> Result<Option<WorkOrder>, DbError> {
    Ok(sqlx::query!(
        "SELECT * FROM work_orders WHERE issue_id = $1 OR mr_id = $2
         ORDER BY created_at DESC, id DESC LIMIT 1",
        issue,
        mr
    )
    .fetch_optional(exec)
    .await?
    .map(|r| order!(r)))
}

/// Ends an open order; false if it had ended already.
pub async fn end(
    exec: impl PgExecutor<'_>,
    id: uuid::Uuid,
    reason: &str,
    by: Option<UserId>,
) -> Result<bool, DbError> {
    let r = sqlx::query!(
        "UPDATE work_orders SET state = 'ended', end_reason = $2, ended_by = $3, ended_at = now()
         WHERE id = $1 AND state = 'open'",
        id,
        reason,
        by.map(|u| *u.as_uuid())
    )
    .execute(exec)
    .await?;
    Ok(r.rows_affected() == 1)
}

/// Records a decision under an order.
pub async fn insert_event(
    exec: impl PgExecutor<'_>,
    order: uuid::Uuid,
    (action, decision, policy): (&str, &str, Option<&str>),
    target: Option<&str>,
    actor: Option<UserId>,
) -> Result<(), DbError> {
    sqlx::query!(
        "INSERT INTO work_order_events (id, order_id, action, decision, policy, target, actor_id)
         VALUES ($1, $2, $3, $4, $5, $6, $7)",
        uuid::Uuid::now_v7(),
        order,
        action,
        decision,
        policy,
        target,
        actor.map(|u| *u.as_uuid())
    )
    .execute(exec)
    .await?;
    Ok(())
}

/// An order's events, oldest first.
pub async fn events(
    exec: impl PgExecutor<'_>,
    order: uuid::Uuid,
    offset: i64,
    limit: i64,
) -> Result<Vec<Event>, DbError> {
    Ok(sqlx::query!(
        "SELECT id, action, decision, policy, target, actor_id, created_at
           FROM work_order_events WHERE order_id = $1
          ORDER BY created_at, id OFFSET $2 LIMIT $3",
        order,
        offset,
        limit
    )
    .fetch_all(exec)
    .await?
    .into_iter()
    .map(|r| Event {
        id: r.id,
        action: r.action,
        decision: r.decision,
        policy: r.policy,
        target: r.target,
        actor_id: r.actor_id.map(UserId::from_uuid),
        created_at: r.created_at,
    })
    .collect())
}

/// A task token that may still be used, with its order.
#[derive(Debug, Clone)]
pub struct ActiveTaskToken {
    pub id: uuid::Uuid,
    pub order_id: uuid::Uuid,
    pub repo_id: RepoId,
    pub bot_id: UserId,
    pub last_used_at: Option<OffsetDateTime>,
}

/// Issues a task token for an open order: revokes the order's earlier
/// ones, stores the new one, and marks the order claimed. Run it in a
/// transaction.
pub async fn claim(
    tx: &mut sqlx::PgConnection,
    order: uuid::Uuid,
    (prefix, hash): (&str, &[u8]),
    expires_at: OffsetDateTime,
) -> Result<uuid::Uuid, DbError> {
    sqlx::query!(
        "UPDATE task_tokens SET revoked_at = now() WHERE order_id = $1 AND revoked_at IS NULL",
        order
    )
    .execute(&mut *tx)
    .await?;
    let id = uuid::Uuid::now_v7();
    sqlx::query!(
        "INSERT INTO task_tokens (id, order_id, token_prefix, token_hash, expires_at)
         VALUES ($1, $2, $3, $4, $5)",
        id,
        order,
        prefix,
        hash,
        expires_at
    )
    .execute(&mut *tx)
    .await?;
    sqlx::query!(
        "UPDATE work_orders SET claimed_at = coalesce(claimed_at, now()) WHERE id = $1",
        order
    )
    .execute(&mut *tx)
    .await?;
    Ok(id)
}

/// The task token with this storage hash, if it's unrevoked, unexpired,
/// and its order is open and within its time.
pub async fn find_task_token(
    exec: impl PgExecutor<'_>,
    hash: &[u8],
) -> Result<Option<ActiveTaskToken>, DbError> {
    Ok(sqlx::query!(
        "SELECT t.id, t.order_id, o.repo_id, o.bot_id, t.last_used_at
           FROM task_tokens t JOIN work_orders o ON o.id = t.order_id
          WHERE t.token_hash = $1 AND t.revoked_at IS NULL AND t.expires_at > now()
            AND o.state = 'open' AND o.expires_at > now()",
        hash
    )
    .fetch_optional(exec)
    .await?
    .map(|r| ActiveTaskToken {
        id: r.id,
        order_id: r.order_id,
        repo_id: RepoId::from_uuid(r.repo_id),
        bot_id: UserId::from_uuid(r.bot_id),
        last_used_at: r.last_used_at,
    }))
}

/// Records that a task token was used.
pub async fn touch_task_token(exec: impl PgExecutor<'_>, id: uuid::Uuid) -> Result<(), DbError> {
    sqlx::query!(
        "UPDATE task_tokens SET last_used_at = now() WHERE id = $1",
        id
    )
    .execute(exec)
    .await?;
    Ok(())
}

/// How many merge requests of the repository whose source is a branch
/// under `prefix` (a full ref name such as `refs/heads/wo/3/`) are open,
/// and how many revisions all of them have, open or not.
pub async fn order_mr_usage(
    exec: impl PgExecutor<'_>,
    repo: RepoId,
    prefix: &[u8],
) -> Result<(i64, i64), DbError> {
    let r = sqlx::query!(
        r#"SELECT count(*) FILTER (WHERE m.state = 'open') AS "open!",
                  coalesce(sum((SELECT count(*) FROM mr_revisions r WHERE r.mr_id = m.id)), 0)::bigint
                    AS "revisions!"
             FROM merge_requests m
            WHERE m.repo_id = $1 AND m.source_repo_id = $1 AND position($2::bytea in m.source_ref) = 1"#,
        repo.as_uuid(),
        prefix
    )
    .fetch_one(exec)
    .await?;
    Ok((r.open, r.revisions))
}

/// Whether `oid` is the head of a revision of a merge request whose
/// source is a branch under `prefix` in the repository.
pub async fn is_order_head(
    exec: impl PgExecutor<'_>,
    repo: RepoId,
    prefix: &[u8],
    oid: &str,
) -> Result<bool, DbError> {
    Ok(sqlx::query_scalar!(
        r#"SELECT EXISTS (
             SELECT 1 FROM mr_revisions r JOIN merge_requests m ON m.id = r.mr_id
              WHERE m.repo_id = $1 AND m.source_repo_id = $1
                AND position($2::bytea in m.source_ref) = 1 AND r.head_oid = $3) AS "e!""#,
        repo.as_uuid(),
        prefix,
        oid
    )
    .fetch_one(exec)
    .await?)
}

/// The source refs of the open merge requests whose source is a branch
/// under `prefix` in the repository.
pub async fn open_order_sources(
    exec: impl PgExecutor<'_>,
    repo: RepoId,
    prefix: &[u8],
) -> Result<Vec<Vec<u8>>, DbError> {
    Ok(sqlx::query_scalar!(
        "SELECT source_ref FROM merge_requests
          WHERE repo_id = $1 AND source_repo_id = $1 AND state = 'open'
            AND position($2::bytea in source_ref) = 1",
        repo.as_uuid(),
        prefix
    )
    .fetch_all(exec)
    .await?)
}
