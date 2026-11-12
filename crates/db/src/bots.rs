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
        }
    };
}

/// A bot by name (case-insensitive).
pub async fn find(exec: impl PgExecutor<'_>, name: &str) -> Result<Option<Bot>, DbError> {
    let r = sqlx::query!(
        r#"SELECT u.id, u.namespace_id, n.name::text AS "name!", u.display_name, u.state,
                  u.bot_owner_kind AS "owner_kind!", u.bot_owner_id AS "owner_id!",
                  o.name::text AS "owner_name!", u.created_at
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
                  o.name::text AS "owner_name!", u.created_at
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
