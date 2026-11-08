// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Changes of merge requests: changes, their versions per revision, and
//! the aliases that name them.

use slath_core::ids::{MergeRequestId, UserId};
use sqlx::PgExecutor;

use crate::DbError;

/// A change of a merge request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    pub id: uuid::Uuid,
    pub display_id: String,
    /// `active`, `removed`, or `merged`.
    pub state: String,
}

/// A version: one commit of a revision, bound to a change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Version {
    pub id: uuid::Uuid,
    pub revision_seq: i32,
    /// `None` while unresolved.
    pub change_id: Option<uuid::Uuid>,
    pub position: i32,
    pub commit_oid: String,
    pub parent_oid: Option<String>,
    pub patch_id: Option<String>,
    /// `new`, `confirmed`, `inferred`, or `unresolved`.
    pub state: String,
    pub reason: Option<String>,
    pub candidates: Vec<uuid::Uuid>,
    pub conflicted: bool,
}

/// An alias bound to a change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoundAlias {
    pub kind: String,
    pub value: String,
    pub change_id: uuid::Uuid,
}

pub async fn changes(
    exec: impl PgExecutor<'_>,
    mr: MergeRequestId,
) -> Result<Vec<Change>, DbError> {
    let rows = sqlx::query!(
        "SELECT id, display_id, state FROM mr_changes WHERE mr_id = $1 ORDER BY created_at, id",
        mr.as_uuid()
    )
    .fetch_all(exec)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| Change {
            id: r.id,
            display_id: r.display_id,
            state: r.state,
        })
        .collect())
}

/// Every version of every revision, by revision and position.
pub async fn versions(
    exec: impl PgExecutor<'_>,
    mr: MergeRequestId,
) -> Result<Vec<Version>, DbError> {
    let rows = sqlx::query!(
        r#"SELECT c.id, r.seq, c.change_id, c.position, c.commit_oid, c.parent_oid, c.patch_id,
                  c.state, c.reason, c.candidates, c.conflicted
           FROM mr_change_versions c JOIN mr_revisions r ON r.id = c.revision_id
           WHERE r.mr_id = $1 ORDER BY r.seq, c.position"#,
        mr.as_uuid()
    )
    .fetch_all(exec)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| Version {
            id: r.id,
            revision_seq: r.seq,
            change_id: r.change_id,
            position: r.position,
            commit_oid: r.commit_oid,
            parent_oid: r.parent_oid,
            patch_id: r.patch_id,
            state: r.state,
            reason: r.reason,
            candidates: r.candidates,
            conflicted: r.conflicted,
        })
        .collect())
}

pub async fn aliases(
    exec: impl PgExecutor<'_>,
    mr: MergeRequestId,
) -> Result<Vec<BoundAlias>, DbError> {
    let rows = sqlx::query!(
        "SELECT kind, value, change_id FROM mr_change_aliases WHERE mr_id = $1",
        mr.as_uuid()
    )
    .fetch_all(exec)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| BoundAlias {
            kind: r.kind,
            value: r.value,
            change_id: r.change_id,
        })
        .collect())
}

pub async fn insert_change(
    exec: impl PgExecutor<'_>,
    mr: MergeRequestId,
    display_id: &str,
) -> Result<uuid::Uuid, DbError> {
    let id = uuid::Uuid::now_v7();
    sqlx::query!(
        "INSERT INTO mr_changes (id, mr_id, display_id) VALUES ($1, $2, $3)",
        id,
        mr.as_uuid(),
        display_id
    )
    .execute(exec)
    .await?;
    Ok(id)
}

pub async fn set_change_state(
    exec: impl PgExecutor<'_>,
    change: uuid::Uuid,
    state: &str,
) -> Result<(), DbError> {
    sqlx::query!(
        "UPDATE mr_changes SET state = $2 WHERE id = $1",
        change,
        state
    )
    .execute(exec)
    .await?;
    Ok(())
}

/// Binds an alias to a change, or moves it there.
pub async fn bind_alias(
    exec: impl PgExecutor<'_>,
    mr: MergeRequestId,
    (kind, value): (&str, &str),
    change: uuid::Uuid,
    revision_seq: i32,
    by: Option<UserId>,
) -> Result<(), DbError> {
    sqlx::query!(
        "INSERT INTO mr_change_aliases (mr_id, kind, value, change_id, revision_seq, bound_by)
         VALUES ($1, $2, $3, $4, $5, $6)
         ON CONFLICT (mr_id, kind, value) DO UPDATE
             SET change_id = EXCLUDED.change_id, revision_seq = EXCLUDED.revision_seq,
                 bound_by = EXCLUDED.bound_by, created_at = now()",
        mr.as_uuid(),
        kind,
        value,
        change,
        revision_seq,
        by.map(|u| *u.as_uuid())
    )
    .execute(exec)
    .await?;
    Ok(())
}

/// What a new version records.
#[derive(Debug)]
pub struct NewVersion<'a> {
    pub revision: uuid::Uuid,
    pub change: Option<uuid::Uuid>,
    pub position: i32,
    pub commit_oid: &'a str,
    pub parent_oid: Option<&'a str>,
    pub patch_id: Option<&'a str>,
    pub state: &'a str,
    pub reason: Option<&'a str>,
    pub candidates: &'a [uuid::Uuid],
    pub conflicted: bool,
}

pub async fn insert_version(
    exec: impl PgExecutor<'_>,
    v: &NewVersion<'_>,
) -> Result<uuid::Uuid, DbError> {
    let id = uuid::Uuid::now_v7();
    sqlx::query!(
        "INSERT INTO mr_change_versions (id, revision_id, change_id, position, commit_oid,
             parent_oid, patch_id, state, reason, candidates, conflicted)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)",
        id,
        v.revision,
        v.change,
        v.position,
        v.commit_oid,
        v.parent_oid,
        v.patch_id,
        v.state,
        v.reason,
        v.candidates,
        v.conflicted
    )
    .execute(exec)
    .await?;
    Ok(id)
}

/// Rebinds a version (split, join, resolve).
pub async fn set_version(
    exec: impl PgExecutor<'_>,
    version: uuid::Uuid,
    change: uuid::Uuid,
    state: &str,
    reason: &str,
) -> Result<(), DbError> {
    sqlx::query!(
        "UPDATE mr_change_versions SET change_id = $2, state = $3, reason = $4, candidates = '{}'
         WHERE id = $1",
        version,
        change,
        state,
        reason
    )
    .execute(exec)
    .await?;
    Ok(())
}

/// Moves the aliases of `from` to `to` and deletes `from` (join).
pub async fn merge_into(
    exec: &mut sqlx::PgConnection,
    from: uuid::Uuid,
    to: uuid::Uuid,
) -> Result<(), DbError> {
    sqlx::query!(
        "UPDATE mr_change_versions SET change_id = $2 WHERE change_id = $1",
        from,
        to
    )
    .execute(&mut *exec)
    .await?;
    sqlx::query!(
        "UPDATE mr_change_aliases SET change_id = $2 WHERE change_id = $1",
        from,
        to
    )
    .execute(&mut *exec)
    .await?;
    sqlx::query!("DELETE FROM mr_changes WHERE id = $1", from)
        .execute(&mut *exec)
        .await?;
    Ok(())
}

/// Other open merge requests of the repository (number) that bound one of
/// these aliases ("also in !m").
pub async fn also_in(
    exec: impl PgExecutor<'_>,
    mr: MergeRequestId,
    kinds: &[String],
    values: &[String],
) -> Result<Vec<(String, String, i32)>, DbError> {
    let rows = sqlx::query!(
        r#"SELECT a.kind, a.value, o.number
           FROM mr_change_aliases a
           JOIN merge_requests o ON o.id = a.mr_id
           JOIN merge_requests m ON m.id = $1
           JOIN unnest($2::text[], $3::text[]) AS x(kind, value) ON x.kind = a.kind AND x.value = a.value
           WHERE o.repo_id = m.repo_id AND o.id <> m.id AND o.state = 'open'
           ORDER BY o.number"#,
        mr.as_uuid(),
        kinds,
        values
    )
    .fetch_all(exec)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| (r.kind, r.value, r.number))
        .collect())
}
