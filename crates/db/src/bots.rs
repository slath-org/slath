// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Bot accounts: users of kind `bot`, owned by a user or an organization.

use slath_core::authz::UserState;
use slath_core::ids::{NamespaceId, OrgId, UserId};
use sqlx::PgExecutor;
use time::OffsetDateTime;

use crate::DbError;

/// Who owns a bot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BotOwner {
    User(UserId),
    Org(OrgId),
}

impl BotOwner {
    fn parts(self) -> (&'static str, uuid::Uuid) {
        match self {
            Self::User(u) => ("user", *u.as_uuid()),
            Self::Org(o) => ("org", *o.as_uuid()),
        }
    }

    fn from_parts(kind: &str, id: uuid::Uuid) -> Self {
        if kind == "org" {
            Self::Org(OrgId::from_uuid(id))
        } else {
            Self::User(UserId::from_uuid(id))
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bot {
    pub id: UserId,
    pub namespace_id: NamespaceId,
    pub name: String,
    pub display_name: Option<String>,
    pub state: UserState,
    pub owner: BotOwner,
    /// The owner's namespace name (user or organization).
    pub owner_name: String,
    pub created_at: OffsetDateTime,
    /// Set while it's stopped by a manager or an administrator.
    pub stopped_at: Option<OffsetDateTime>,
}

/// Creates the namespace and the bot; run it in a transaction. A taken or
/// reserved name fails with [`DbError::Conflict`].
pub async fn create(
    tx: &mut sqlx::PgConnection,
    name: &str,
    display_name: Option<&str>,
    owner: BotOwner,
) -> Result<UserId, DbError> {
    let reserved: bool = sqlx::query_scalar!(
        r#"SELECT EXISTS (SELECT 1 FROM reserved_names WHERE name = $1::citext) AS "r!""#,
        name
    )
    .fetch_one(&mut *tx)
    .await?;
    if reserved {
        return Err(DbError::Conflict("name"));
    }
    let ns = NamespaceId::new();
    let id = UserId::new();
    sqlx::query!(
        "INSERT INTO namespaces (id, name, kind) VALUES ($1, $2, 'user')",
        ns.as_uuid(),
        name
    )
    .execute(&mut *tx)
    .await
    .map_err(|e| DbError::from_unique(e, "name"))?;
    let (kind, owner_id) = owner.parts();
    sqlx::query!(
        "INSERT INTO users (id, namespace_id, kind, display_name, bot_owner_kind, bot_owner_id)
         VALUES ($1, $2, 'bot', $3, $4, $5)",
        id.as_uuid(),
        ns.as_uuid(),
        display_name,
        kind,
        owner_id,
    )
    .execute(&mut *tx)
    .await?;
    Ok(id)
}

macro_rules! to_bot {
    ($r:expr) => {
        Bot {
            id: UserId::from_uuid($r.id),
            namespace_id: NamespaceId::from_uuid($r.namespace_id),
            name: $r.name,
            display_name: $r.display_name,
            state: crate::users::state(&$r.state),
            owner: BotOwner::from_parts(&$r.owner_kind, $r.owner_id),
            owner_name: $r.owner_name,
            created_at: $r.created_at,
            stopped_at: $r.stopped_at,
        }
    };
}

/// A bot by name (case-insensitive).
pub async fn find(exec: impl PgExecutor<'_>, name: &str) -> Result<Option<Bot>, DbError> {
    let r = sqlx::query!(
        r#"SELECT u.id, u.namespace_id, n.name::text AS "name!", u.display_name, u.state,
                  u.bot_owner_kind AS "owner_kind!", u.bot_owner_id AS "owner_id!",
                  o.name::text AS "owner_name!", u.created_at, u.stopped_at
           FROM users u JOIN namespaces n ON n.id = u.namespace_id
           JOIN namespaces o ON o.id = CASE u.bot_owner_kind
                WHEN 'user' THEN (SELECT namespace_id FROM users WHERE id = u.bot_owner_id)
                ELSE (SELECT namespace_id FROM organizations WHERE id = u.bot_owner_id) END
           WHERE n.name = $1::citext AND u.kind = 'bot'"#,
        name
    )
    .fetch_optional(exec)
    .await?;
    Ok(r.map(|r| to_bot!(r)))
}

/// The bots of one owner, by name.
pub async fn list_for_owner(
    exec: impl PgExecutor<'_>,
    owner: BotOwner,
) -> Result<Vec<Bot>, DbError> {
    let (kind, owner_id) = owner.parts();
    let rows = sqlx::query!(
        r#"SELECT u.id, u.namespace_id, n.name::text AS "name!", u.display_name, u.state,
                  u.bot_owner_kind AS "owner_kind!", u.bot_owner_id AS "owner_id!",
                  o.name::text AS "owner_name!", u.created_at, u.stopped_at
           FROM users u JOIN namespaces n ON n.id = u.namespace_id
           JOIN namespaces o ON o.id = CASE u.bot_owner_kind
                WHEN 'user' THEN (SELECT namespace_id FROM users WHERE id = u.bot_owner_id)
                ELSE (SELECT namespace_id FROM organizations WHERE id = u.bot_owner_id) END
           WHERE u.kind = 'bot' AND u.bot_owner_kind = $1 AND u.bot_owner_id = $2
           ORDER BY n.name LIMIT 1000"#,
        kind,
        owner_id
    )
    .fetch_all(exec)
    .await?;
    Ok(rows.into_iter().map(|r| to_bot!(r)).collect())
}

/// Suspends the active bots of a user who is being suspended; returns how
/// many.
pub async fn suspend_with_owner(exec: impl PgExecutor<'_>, owner: UserId) -> Result<u64, DbError> {
    let r = sqlx::query!(
        "UPDATE users SET state = 'suspended', suspended_with_owner = true, updated_at = now()
         WHERE kind = 'bot' AND bot_owner_kind = 'user' AND bot_owner_id = $1 AND state = 'active'",
        owner.as_uuid()
    )
    .execute(exec)
    .await?;
    Ok(r.rows_affected())
}

/// Reactivates the bots that were suspended with their owner; returns how
/// many.
pub async fn reactivate_with_owner(
    exec: impl PgExecutor<'_>,
    owner: UserId,
) -> Result<u64, DbError> {
    let r = sqlx::query!(
        "UPDATE users SET state = 'active', suspended_with_owner = false, updated_at = now()
         WHERE kind = 'bot' AND bot_owner_kind = 'user' AND bot_owner_id = $1
           AND state = 'suspended' AND suspended_with_owner",
        owner.as_uuid()
    )
    .execute(exec)
    .await?;
    Ok(r.rows_affected())
}

/// Every bot's name, lowercased (for marking bots wherever names appear).
pub async fn all_names(exec: impl PgExecutor<'_>) -> Result<Vec<String>, DbError> {
    Ok(sqlx::query_scalar!(
        r#"SELECT lower(n.name::text) AS "name!" FROM users u JOIN namespaces n ON n.id = u.namespace_id
           WHERE u.kind = 'bot'"#
    )
    .fetch_all(exec)
    .await?)
}

/// Stops a bot: suspends it and removes its tokens and SSH keys, in the
/// caller's transaction. Returns (tokens revoked, keys removed), or `None`
/// if it was stopped already.
pub async fn stop(
    tx: &mut sqlx::PgConnection,
    bot: UserId,
    by: Option<UserId>,
) -> Result<Option<(u64, u64)>, DbError> {
    let r = sqlx::query!(
        "UPDATE users SET state = 'suspended', suspended_with_owner = false, stopped_at = now(),
             stopped_by = $2, updated_at = now()
         WHERE id = $1 AND kind = 'bot' AND state <> 'deleted' AND stopped_at IS NULL",
        bot.as_uuid(),
        by.map(|u| *u.as_uuid())
    )
    .execute(&mut *tx)
    .await?;
    if r.rows_affected() == 0 {
        return Ok(None);
    }
    let tokens = crate::tokens::revoke_all_for_user(&mut *tx, bot).await?;
    let keys = sqlx::query!("DELETE FROM ssh_keys WHERE user_id = $1", bot.as_uuid())
        .execute(&mut *tx)
        .await?
        .rows_affected();
    Ok(Some((tokens, keys)))
}

/// Re-enables a stopped bot (without credentials); false if it wasn't
/// stopped.
pub async fn start(exec: impl PgExecutor<'_>, bot: UserId) -> Result<bool, DbError> {
    let r = sqlx::query!(
        "UPDATE users SET state = 'active', stopped_at = NULL, stopped_by = NULL, updated_at = now()
         WHERE id = $1 AND kind = 'bot' AND stopped_at IS NOT NULL",
        bot.as_uuid()
    )
    .execute(exec)
    .await?;
    Ok(r.rows_affected() > 0)
}

/// A freeze of every bot of the instance (`org: None`) or of one
/// organization.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Freeze {
    pub frozen_by: Option<String>,
    pub frozen_at: OffsetDateTime,
}

/// The freeze of a scope, if there is one.
pub async fn freeze(
    exec: impl PgExecutor<'_>,
    org: Option<OrgId>,
) -> Result<Option<Freeze>, DbError> {
    let r = sqlx::query!(
        r#"SELECT n.name::text AS frozen_by, f.frozen_at
           FROM bot_freezes f
           LEFT JOIN users u ON u.id = f.frozen_by AND u.state <> 'deleted'
           LEFT JOIN namespaces n ON n.id = u.namespace_id
           WHERE f.org_id IS NOT DISTINCT FROM $1"#,
        org.map(|o| *o.as_uuid())
    )
    .fetch_optional(exec)
    .await?;
    Ok(r.map(|r| Freeze {
        frozen_by: r.frozen_by,
        frozen_at: r.frozen_at,
    }))
}

/// Freezes a scope; false if it was frozen already.
pub async fn set_freeze(
    exec: impl PgExecutor<'_>,
    org: Option<OrgId>,
    by: Option<UserId>,
) -> Result<bool, DbError> {
    let r = sqlx::query!(
        "INSERT INTO bot_freezes (org_id, frozen_by) VALUES ($1, $2) ON CONFLICT DO NOTHING",
        org.map(|o| *o.as_uuid()),
        by.map(|u| *u.as_uuid())
    )
    .execute(exec)
    .await?;
    Ok(r.rows_affected() > 0)
}

/// Ends a scope's freeze; false if it wasn't frozen.
pub async fn clear_freeze(exec: impl PgExecutor<'_>, org: Option<OrgId>) -> Result<bool, DbError> {
    let r = sqlx::query!(
        "DELETE FROM bot_freezes WHERE org_id IS NOT DISTINCT FROM $1",
        org.map(|o| *o.as_uuid())
    )
    .execute(exec)
    .await?;
    Ok(r.rows_affected() > 0)
}

/// True if a freeze applies to this account (always false for people).
pub async fn frozen(exec: impl PgExecutor<'_>, user: UserId) -> Result<bool, DbError> {
    Ok(sqlx::query_scalar!(
        r#"SELECT slath_bot_frozen($1) AS "frozen!""#,
        user.as_uuid()
    )
    .fetch_one(exec)
    .await?)
}
