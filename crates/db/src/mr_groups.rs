// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Linked merge requests: ordered groups of merge requests that merge
//! together. A group's state follows from its members; the table only
//! holds why its last group merge stopped and whether one is running.

use slath_core::ids::{MergeRequestId, UserId};
use sqlx::{PgConnection, PgExecutor};

use crate::DbError;

/// A group and its members in order (every member, readable or not).
#[derive(Debug, Clone)]
pub struct Group {
    pub id: uuid::Uuid,
    /// `{mr_id, reason, at, actor}` of the last group merge that stopped.
    pub last_failure: Option<serde_json::Value>,
    pub members: Vec<MergeRequestId>,
}

/// The group `mr` belongs to.
pub async fn of(exec: &mut PgConnection, mr: MergeRequestId) -> Result<Option<Group>, DbError> {
    let Some(g) = sqlx::query!(
        "SELECT g.id, g.last_failure FROM mr_groups g
           JOIN mr_group_members m ON m.group_id = g.id
          WHERE m.mr_id = $1",
        mr.as_uuid()
    )
    .fetch_optional(&mut *exec)
    .await?
    else {
        return Ok(None);
    };
    let members = sqlx::query_scalar!(
        "SELECT mr_id FROM mr_group_members WHERE group_id = $1 ORDER BY position",
        g.id
    )
    .fetch_all(&mut *exec)
    .await?
    .into_iter()
    .map(MergeRequestId::from_uuid)
    .collect();
    Ok(Some(Group {
        id: g.id,
        last_failure: g.last_failure,
        members,
    }))
}

/// Locks the group's row until the transaction ends.
pub async fn lock(exec: impl PgExecutor<'_>, group: uuid::Uuid) -> Result<(), DbError> {
    sqlx::query!("SELECT id FROM mr_groups WHERE id = $1 FOR UPDATE", group)
        .fetch_optional(exec)
        .await?;
    Ok(())
}

/// Creates an empty group.
pub async fn create(exec: impl PgExecutor<'_>, by: Option<UserId>) -> Result<uuid::Uuid, DbError> {
    let id = uuid::Uuid::now_v7();
    sqlx::query!(
        "INSERT INTO mr_groups (id, created_by) VALUES ($1, $2)",
        id,
        by.map(|u| *u.as_uuid())
    )
    .execute(exec)
    .await?;
    Ok(id)
}

/// Replaces the group's members with `members`, in this order, and clears
/// its last failure. A member of another group fails with a unique
/// violation, so callers check membership under the group's lock first.
pub async fn set_members(
    exec: &mut PgConnection,
    group: uuid::Uuid,
    members: &[MergeRequestId],
) -> Result<(), DbError> {
    sqlx::query!("DELETE FROM mr_group_members WHERE group_id = $1", group)
        .execute(&mut *exec)
        .await?;
    let ids: Vec<uuid::Uuid> = members.iter().map(|m| *m.as_uuid()).collect();
    sqlx::query!(
        "INSERT INTO mr_group_members (group_id, mr_id, position)
         SELECT $1, m.id, m.ord::int - 1 FROM unnest($2::uuid[]) WITH ORDINALITY AS m(id, ord)",
        group,
        &ids
    )
    .execute(&mut *exec)
    .await?;
    sqlx::query!(
        "UPDATE mr_groups SET last_failure = NULL WHERE id = $1",
        group
    )
    .execute(&mut *exec)
    .await?;
    Ok(())
}

/// Deletes the group with its memberships.
pub async fn delete(exec: impl PgExecutor<'_>, group: uuid::Uuid) -> Result<(), DbError> {
    sqlx::query!("DELETE FROM mr_groups WHERE id = $1", group)
        .execute(exec)
        .await?;
    Ok(())
}

/// Claims the group for a group merge; `false` when one runs (claims older
/// than ten minutes count as abandoned).
pub async fn claim(exec: impl PgExecutor<'_>, group: uuid::Uuid) -> Result<bool, DbError> {
    Ok(sqlx::query!(
        "UPDATE mr_groups SET merging_since = now()
          WHERE id = $1 AND (merging_since IS NULL OR merging_since < now() - interval '10 minutes')",
        group
    )
    .execute(exec)
    .await?
    .rows_affected()
        == 1)
}

/// Whether a group merge runs now.
pub async fn merging(exec: impl PgExecutor<'_>, group: uuid::Uuid) -> Result<bool, DbError> {
    Ok(sqlx::query_scalar!(
        r#"SELECT (merging_since IS NOT NULL AND merging_since >= now() - interval '10 minutes') AS "m!"
             FROM mr_groups WHERE id = $1"#,
        group
    )
    .fetch_optional(exec)
    .await?
    .unwrap_or(false))
}

/// Ends a group merge, recording why it stopped (`None`: it landed every
/// open member).
pub async fn finish(
    exec: impl PgExecutor<'_>,
    group: uuid::Uuid,
    failure: Option<&serde_json::Value>,
) -> Result<(), DbError> {
    sqlx::query!(
        "UPDATE mr_groups SET merging_since = NULL, last_failure = $2 WHERE id = $1",
        group,
        failure
    )
    .execute(exec)
    .await?;
    Ok(())
}
