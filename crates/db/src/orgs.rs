// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Organizations, their members, and namespace lookups.

use slath_core::ids::{NamespaceId, OrgId, UserId};
use sqlx::PgExecutor;
use time::OffsetDateTime;

use crate::DbError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OrgRole {
    Owner,
    Member,
}

impl OrgRole {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Owner => "owner",
            Self::Member => "member",
        }
    }

    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "owner" => Some(Self::Owner),
            "member" => Some(Self::Member),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Org {
    pub id: OrgId,
    pub namespace_id: NamespaceId,
    pub name: String,
    pub display_name: Option<String>,
    pub description: Option<String>,
    pub default_member_role: String,
    /// Members need a second factor to use the organization's repositories.
    pub require_mfa: bool,
    pub created_at: OffsetDateTime,
}

#[derive(Debug, Clone)]
pub struct Member {
    pub user_id: UserId,
    pub name: String,
    pub display_name: Option<String>,
    pub role: OrgRole,
}

/// What a namespace name refers to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NamespaceKind {
    User(UserId),
    Org(OrgId),
}

/// Resolves a namespace name (case-insensitive) to its user or organization,
/// with the canonical spelling of the name.
pub async fn resolve_namespace(
    exec: impl PgExecutor<'_>,
    name: &str,
) -> Result<Option<(NamespaceId, String, NamespaceKind)>, DbError> {
    let r = sqlx::query!(
        r#"SELECT n.id, n.name::text AS "name!", n.kind, u.id AS "user_id?", o.id AS "org_id?"
           FROM namespaces n
           LEFT JOIN users u ON u.namespace_id = n.id
           LEFT JOIN organizations o ON o.namespace_id = n.id
           WHERE n.name = $1::citext"#,
        name
    )
    .fetch_optional(exec)
    .await?;
    Ok(r.and_then(|r| {
        let kind = match (r.kind.as_str(), r.user_id, r.org_id) {
            ("user", Some(u), _) => NamespaceKind::User(UserId::from_uuid(u)),
            ("org", _, Some(o)) => NamespaceKind::Org(OrgId::from_uuid(o)),
            _ => return None,
        };
        Some((NamespaceId::from_uuid(r.id), r.name, kind))
    }))
}

/// Creates the namespace, the organization, and its first owner. Run in a
/// transaction. A taken or reserved name is [`DbError::Conflict`].
pub async fn create(
    tx: &mut sqlx::PgConnection,
    name: &str,
    display_name: Option<&str>,
    owner: UserId,
) -> Result<OrgId, DbError> {
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
    let id = OrgId::new();
    sqlx::query!(
        "INSERT INTO namespaces (id, name, kind) VALUES ($1, $2, 'org')",
        ns.as_uuid(),
        name
    )
    .execute(&mut *tx)
    .await
    .map_err(|e| DbError::from_unique(e, "name"))?;
    sqlx::query!(
        "INSERT INTO organizations (id, namespace_id, display_name) VALUES ($1, $2, $3)",
        id.as_uuid(),
        ns.as_uuid(),
        display_name
    )
    .execute(&mut *tx)
    .await?;
    sqlx::query!(
        "INSERT INTO org_members (org_id, user_id, role) VALUES ($1, $2, 'owner')",
        id.as_uuid(),
        owner.as_uuid()
    )
    .execute(&mut *tx)
    .await?;
    Ok(id)
}

pub async fn get(exec: impl PgExecutor<'_>, id: OrgId) -> Result<Option<Org>, DbError> {
    let r = sqlx::query!(
        r#"SELECT o.id, o.namespace_id, n.name::text AS "name!", o.display_name, o.description, o.default_member_role, o.require_mfa, o.created_at
           FROM organizations o JOIN namespaces n ON n.id = o.namespace_id WHERE o.id = $1"#,
        id.as_uuid()
    )
    .fetch_optional(exec)
    .await?;
    Ok(r.map(|r| Org {
        id: OrgId::from_uuid(r.id),
        namespace_id: NamespaceId::from_uuid(r.namespace_id),
        name: r.name,
        display_name: r.display_name,
        description: r.description,
        default_member_role: r.default_member_role,
        require_mfa: r.require_mfa,
        created_at: r.created_at,
    }))
}

/// Sets the member policies: the role every member gets, and whether
/// members need a second factor.
pub async fn set_policies(
    exec: impl PgExecutor<'_>,
    id: OrgId,
    default_member_role: &str,
    require_mfa: bool,
) -> Result<(), DbError> {
    sqlx::query!(
        "UPDATE organizations SET default_member_role = $2, require_mfa = $3, updated_at = now() WHERE id = $1",
        id.as_uuid(),
        default_member_role,
        require_mfa
    )
    .execute(exec)
    .await?;
    Ok(())
}

pub async fn update(
    exec: impl PgExecutor<'_>,
    id: OrgId,
    display_name: Option<&str>,
    description: Option<&str>,
) -> Result<(), DbError> {
    sqlx::query!(
        "UPDATE organizations SET display_name = $2, description = $3, updated_at = now() WHERE id = $1",
        id.as_uuid(),
        display_name,
        description
    )
    .execute(exec)
    .await?;
    Ok(())
}

pub async fn role_of(
    exec: impl PgExecutor<'_>,
    org: OrgId,
    user: UserId,
) -> Result<Option<OrgRole>, DbError> {
    let r = sqlx::query_scalar!(
        "SELECT role FROM org_members WHERE org_id = $1 AND user_id = $2",
        org.as_uuid(),
        user.as_uuid()
    )
    .fetch_optional(exec)
    .await?;
    Ok(r.as_deref().and_then(OrgRole::parse))
}

pub async fn members(exec: impl PgExecutor<'_>, org: OrgId) -> Result<Vec<Member>, DbError> {
    let rows = sqlx::query!(
        r#"SELECT m.user_id, n.name::text AS "name!", u.display_name, m.role
           FROM org_members m JOIN users u ON u.id = m.user_id JOIN namespaces n ON n.id = u.namespace_id
           WHERE m.org_id = $1 ORDER BY m.role DESC, n.name LIMIT 1000"#,
        org.as_uuid()
    )
    .fetch_all(exec)
    .await?;
    Ok(rows
        .into_iter()
        .filter_map(|r| {
            Some(Member {
                user_id: UserId::from_uuid(r.user_id),
                name: r.name,
                display_name: r.display_name,
                role: OrgRole::parse(&r.role)?,
            })
        })
        .collect())
}

/// Locks the organization's membership for a change (serializes owner checks).
pub async fn lock(tx: &mut sqlx::PgConnection, org: OrgId) -> Result<(), DbError> {
    sqlx::query!(
        "SELECT id FROM organizations WHERE id = $1 FOR UPDATE",
        org.as_uuid()
    )
    .fetch_optional(&mut *tx)
    .await?;
    Ok(())
}

pub async fn owner_count(exec: impl PgExecutor<'_>, org: OrgId) -> Result<i64, DbError> {
    Ok(sqlx::query_scalar!(
        r#"SELECT count(*) AS "n!" FROM org_members WHERE org_id = $1 AND role = 'owner'"#,
        org.as_uuid()
    )
    .fetch_one(exec)
    .await?)
}

/// Adds a member or changes their role.
pub async fn upsert_member(
    exec: impl PgExecutor<'_>,
    org: OrgId,
    user: UserId,
    role: OrgRole,
) -> Result<(), DbError> {
    sqlx::query!(
        "INSERT INTO org_members (org_id, user_id, role) VALUES ($1, $2, $3)
         ON CONFLICT (org_id, user_id) DO UPDATE SET role = EXCLUDED.role",
        org.as_uuid(),
        user.as_uuid(),
        role.as_str()
    )
    .execute(exec)
    .await?;
    Ok(())
}

pub async fn remove_member(
    exec: impl PgExecutor<'_>,
    org: OrgId,
    user: UserId,
) -> Result<bool, DbError> {
    let r = sqlx::query!(
        "DELETE FROM org_members WHERE org_id = $1 AND user_id = $2",
        org.as_uuid(),
        user.as_uuid()
    )
    .execute(exec)
    .await?;
    Ok(r.rows_affected() > 0)
}

/// Organizations the user belongs to.
pub async fn for_user(
    exec: impl PgExecutor<'_>,
    user: UserId,
) -> Result<Vec<(String, OrgRole)>, DbError> {
    let rows = sqlx::query!(
        r#"SELECT n.name::text AS "name!", m.role FROM org_members m
           JOIN organizations o ON o.id = m.org_id JOIN namespaces n ON n.id = o.namespace_id
           WHERE m.user_id = $1 ORDER BY n.name LIMIT 1000"#,
        user.as_uuid()
    )
    .fetch_all(exec)
    .await?;
    Ok(rows
        .into_iter()
        .filter_map(|r| Some((r.name, OrgRole::parse(&r.role)?)))
        .collect())
}

/// All organizations, by name.
pub async fn list_all(
    exec: impl PgExecutor<'_>,
    offset: i64,
    limit: i64,
) -> Result<Vec<Org>, DbError> {
    let rows = sqlx::query!(
        r#"SELECT o.id, o.namespace_id, n.name::text AS "name!", o.display_name, o.description, o.default_member_role, o.require_mfa, o.created_at
           FROM organizations o JOIN namespaces n ON n.id = o.namespace_id ORDER BY lower(n.name::text) OFFSET $1 LIMIT $2"#,
        offset,
        limit
    )
    .fetch_all(exec)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| Org {
            id: OrgId::from_uuid(r.id),
            namespace_id: NamespaceId::from_uuid(r.namespace_id),
            name: r.name,
            display_name: r.display_name,
            description: r.description,
            default_member_role: r.default_member_role,
            require_mfa: r.require_mfa,
            created_at: r.created_at,
        })
        .collect())
}
