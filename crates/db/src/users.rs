// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Users and their namespaces.

use slath_core::authz::{UserKind, UserState};
use slath_core::ids::{NamespaceId, UserId};
use sqlx::PgExecutor;
use time::OffsetDateTime;

use crate::DbError;

/// Fields of a new human account.
#[derive(Debug)]
pub struct NewUser<'a> {
    pub name: &'a str,
    pub email: Option<&'a str>,
    pub email_verified: bool,
    pub display_name: Option<&'a str>,
    /// PHC string; `None` for accounts that can't sign in with a password.
    pub password_hash: Option<&'a str>,
    pub is_admin: bool,
}

/// A user as most of the application sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct User {
    pub id: UserId,
    pub namespace_id: NamespaceId,
    pub name: String,
    pub kind: UserKind,
    pub state: UserState,
    pub display_name: Option<String>,
    pub email: Option<String>,
    pub is_admin: bool,
    pub theme: String,
    pub created_at: OffsetDateTime,
}

/// What sign-in needs: the user plus the password hash.
#[derive(Debug)]
pub struct LoginRecord {
    pub user: User,
    pub password_hash: Option<String>,
}

pub(crate) fn kind(s: &str) -> UserKind {
    if s == "bot" {
        UserKind::Bot
    } else {
        UserKind::Human
    }
}

pub(crate) fn state(s: &str) -> UserState {
    match s {
        "active" => UserState::Active,
        "suspended" => UserState::Suspended,
        _ => UserState::Deleted,
    }
}

/// Creates the namespace and the user in one statement sequence; run it in a
/// transaction. A taken name (case-insensitive, users and orgs share the
/// namespace table) or a reserved name fails with [`DbError::Conflict`].
pub async fn create(tx: &mut sqlx::PgConnection, u: &NewUser<'_>) -> Result<UserId, DbError> {
    let reserved: bool = sqlx::query_scalar!(
        r#"SELECT EXISTS (SELECT 1 FROM reserved_names WHERE name = $1::citext) AS "r!""#,
        u.name
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
        u.name
    )
    .execute(&mut *tx)
    .await
    .map_err(|e| DbError::from_unique(e, "name"))?;
    sqlx::query!(
        "INSERT INTO users (id, namespace_id, display_name, email, email_verified, password_hash, is_admin)
         VALUES ($1, $2, $3, $4, $5, $6, $7)",
        id.as_uuid(),
        ns.as_uuid(),
        u.display_name,
        u.email,
        u.email_verified,
        u.password_hash,
        u.is_admin,
    )
    .execute(&mut *tx)
    .await
    .map_err(|e| DbError::from_unique(e, "email"))?;
    Ok(id)
}

/// Number of accounts (any state); 0 means a fresh instance.
pub async fn count(exec: impl PgExecutor<'_>) -> Result<i64, DbError> {
    Ok(sqlx::query_scalar!(r#"SELECT count(*) AS "n!" FROM users"#)
        .fetch_one(exec)
        .await?)
}

/// Looks up an account for sign-in by name (case-insensitive).
pub async fn find_for_login(
    exec: impl PgExecutor<'_>,
    name: &str,
) -> Result<Option<LoginRecord>, DbError> {
    let row = sqlx::query!(
        r#"SELECT u.id, u.namespace_id, n.name::text AS "name!", u.kind, u.state, u.display_name,
                  u.email::text AS email, u.is_admin, u.theme, u.created_at, u.password_hash
           FROM users u JOIN namespaces n ON n.id = u.namespace_id
           WHERE n.name = $1::citext"#,
        name
    )
    .fetch_optional(exec)
    .await?;
    Ok(row.map(|r| LoginRecord {
        user: User {
            id: UserId::from_uuid(r.id),
            namespace_id: NamespaceId::from_uuid(r.namespace_id),
            name: r.name,
            kind: kind(&r.kind),
            state: state(&r.state),
            display_name: r.display_name,
            email: r.email,
            is_admin: r.is_admin,
            theme: r.theme,
            created_at: r.created_at,
        },
        password_hash: r.password_hash,
    }))
}

/// Loads a user by ID.
pub async fn get(exec: impl PgExecutor<'_>, id: UserId) -> Result<Option<User>, DbError> {
    let row = sqlx::query!(
        r#"SELECT u.id, u.namespace_id, n.name::text AS "name!", u.kind, u.state, u.display_name,
                  u.email::text AS email, u.is_admin, u.theme, u.created_at
           FROM users u JOIN namespaces n ON n.id = u.namespace_id
           WHERE u.id = $1"#,
        id.as_uuid()
    )
    .fetch_optional(exec)
    .await?;
    Ok(row.map(|r| User {
        id: UserId::from_uuid(r.id),
        namespace_id: NamespaceId::from_uuid(r.namespace_id),
        name: r.name,
        kind: kind(&r.kind),
        state: state(&r.state),
        display_name: r.display_name,
        email: r.email,
        is_admin: r.is_admin,
        theme: r.theme,
        created_at: r.created_at,
    }))
}

/// Replaces the password hash (rehash on login, password change).
pub async fn set_password_hash(
    exec: impl PgExecutor<'_>,
    id: UserId,
    phc: &str,
) -> Result<(), DbError> {
    sqlx::query!(
        "UPDATE users SET password_hash = $2, updated_at = now() WHERE id = $1 AND kind = 'human'",
        id.as_uuid(),
        phc
    )
    .execute(exec)
    .await?;
    Ok(())
}

/// Updates the profile fields a user edits themselves.
pub async fn update_profile(
    exec: impl PgExecutor<'_>,
    id: UserId,
    display_name: Option<&str>,
) -> Result<(), DbError> {
    sqlx::query!(
        "UPDATE users SET display_name = $2, updated_at = now() WHERE id = $1",
        id.as_uuid(),
        display_name
    )
    .execute(exec)
    .await?;
    Ok(())
}

/// The stored password hash of a user.
pub async fn password_hash(
    exec: impl PgExecutor<'_>,
    id: UserId,
) -> Result<Option<String>, DbError> {
    Ok(sqlx::query_scalar!(
        "SELECT password_hash FROM users WHERE id = $1",
        id.as_uuid()
    )
    .fetch_optional(exec)
    .await?
    .flatten())
}

/// Looks up a user by name (case-insensitive).
pub async fn get_by_name(exec: impl PgExecutor<'_>, name: &str) -> Result<Option<User>, DbError> {
    Ok(find_for_login(exec, name).await?.map(|r| r.user))
}

/// A user row for administrators.
#[derive(Debug, Clone)]
pub struct AdminUser {
    pub user: User,
    pub email_verified: bool,
}

/// All users, by name.
pub async fn list_all(
    exec: impl PgExecutor<'_>,
    offset: i64,
    limit: i64,
) -> Result<Vec<User>, DbError> {
    let rows = sqlx::query!(
        r#"SELECT u.id, u.namespace_id, n.name::text AS "name!", u.kind, u.state, u.display_name,
                  u.email::text AS email, u.is_admin, u.theme, u.created_at
           FROM users u JOIN namespaces n ON n.id = u.namespace_id
           ORDER BY lower(n.name::text) OFFSET $1 LIMIT $2"#,
        offset,
        limit
    )
    .fetch_all(exec)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| User {
            id: UserId::from_uuid(r.id),
            namespace_id: NamespaceId::from_uuid(r.namespace_id),
            name: r.name,
            kind: kind(&r.kind),
            state: state(&r.state),
            display_name: r.display_name,
            email: r.email,
            is_admin: r.is_admin,
            theme: r.theme,
            created_at: r.created_at,
        })
        .collect())
}

pub async fn set_state(exec: impl PgExecutor<'_>, id: UserId, active: bool) -> Result<(), DbError> {
    let s = if active { "active" } else { "suspended" };
    sqlx::query!(
        "UPDATE users SET state = $2, updated_at = now() WHERE id = $1 AND state <> 'deleted'",
        id.as_uuid(),
        s
    )
    .execute(exec)
    .await?;
    Ok(())
}

pub async fn set_admin(exec: impl PgExecutor<'_>, id: UserId, admin: bool) -> Result<(), DbError> {
    sqlx::query!(
        "UPDATE users SET is_admin = $2, updated_at = now() WHERE id = $1 AND kind = 'human'",
        id.as_uuid(),
        admin
    )
    .execute(exec)
    .await?;
    Ok(())
}

/// Saves a user's appearance settings.
pub async fn set_appearance(
    exec: impl PgExecutor<'_>,
    id: UserId,
    theme: &str,
    density: &str,
    code_font_size: i16,
) -> Result<(), DbError> {
    sqlx::query!(
        "UPDATE users SET theme = $2, density = $3, code_font_size = $4, updated_at = now()
         WHERE id = $1",
        id.as_uuid(),
        theme,
        density,
        code_font_size
    )
    .execute(exec)
    .await?;
    Ok(())
}
