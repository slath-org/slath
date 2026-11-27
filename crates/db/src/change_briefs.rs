// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Briefs: what the author of a change version, or of a merge request's
//! revision, says about it. Rows are append-only; a version's (or a
//! revision's) brief is its newest row.

use slath_core::ids::{MergeRequestId, UserId};
use sqlx::PgExecutor;
use time::OffsetDateTime;

use crate::DbError;

/// One edit of a brief.
#[derive(Debug, Clone)]
pub struct Brief {
    pub id: uuid::Uuid,
    pub mr_id: MergeRequestId,
    /// A change version's brief: its change; `None` for a revision's.
    pub change_id: Option<uuid::Uuid>,
    /// A revision's brief: its number.
    pub revision_seq: Option<i32>,
    /// The version's commit, or the revision's head.
    pub commit_oid: String,
    pub author_id: Option<UserId>,
    pub origin: String,
    pub schema: String,
    /// The items (`slath-brief/2`), as the service checked and stored them.
    pub items: serde_json::Value,
    pub created_at: OffsetDateTime,
}

/// A new edit; the caller has checked the items.
#[derive(Debug, Clone)]
pub struct NewBrief<'a> {
    pub mr_id: MergeRequestId,
    pub change_id: Option<uuid::Uuid>,
    pub revision_seq: Option<i32>,
    pub commit_oid: &'a str,
    pub author_id: UserId,
    pub origin: &'a str,
    pub items: &'a serde_json::Value,
}

macro_rules! brief {
    ($r:expr) => {{
        let r = $r;
        Brief {
            id: r.id,
            mr_id: MergeRequestId::from_uuid(r.mr_id),
            change_id: r.change_id,
            revision_seq: r.revision_seq,
            commit_oid: r.commit_oid,
            author_id: r.author_id.map(UserId::from_uuid),
            origin: r.origin,
            schema: r.schema,
            items: r.items,
            created_at: r.created_at,
        }
    }};
}

pub async fn insert(exec: impl PgExecutor<'_>, b: &NewBrief<'_>) -> Result<Brief, DbError> {
    let r = sqlx::query!(
        "INSERT INTO change_briefs (id, mr_id, change_id, revision_seq, commit_oid, author_id,
             origin, schema, items)
         VALUES ($1, $2, $3, $4, $5, $6, $7, 'slath-brief/2', $8)
         RETURNING *",
        uuid::Uuid::now_v7(),
        b.mr_id.as_uuid(),
        b.change_id,
        b.revision_seq,
        b.commit_oid,
        b.author_id.as_uuid(),
        b.origin,
        b.items,
    )
    .fetch_one(exec)
    .await?;
    Ok(brief!(r))
}

/// A change's briefs, newest first; with `commit`, only that version's.
pub async fn list(
    exec: impl PgExecutor<'_>,
    change: uuid::Uuid,
    commit: Option<&str>,
    limit: i64,
) -> Result<Vec<Brief>, DbError> {
    Ok(sqlx::query!(
        "SELECT * FROM change_briefs
          WHERE change_id = $1 AND ($2::text IS NULL OR commit_oid = $2)
          ORDER BY created_at DESC, id DESC
          LIMIT $3",
        change,
        commit,
        limit
    )
    .fetch_all(exec)
    .await?
    .into_iter()
    .map(|r| brief!(r))
    .collect())
}

/// A merge request's revision briefs, newest first; with `revision`, only
/// that revision's.
pub async fn list_for_revisions(
    exec: impl PgExecutor<'_>,
    mr: MergeRequestId,
    revision: Option<i32>,
    limit: i64,
) -> Result<Vec<Brief>, DbError> {
    Ok(sqlx::query!(
        "SELECT * FROM change_briefs
          WHERE mr_id = $1 AND revision_seq IS NOT NULL
            AND ($2::int IS NULL OR revision_seq = $2)
          ORDER BY created_at DESC, id DESC
          LIMIT $3",
        mr.as_uuid(),
        revision,
        limit
    )
    .fetch_all(exec)
    .await?
    .into_iter()
    .map(|r| brief!(r))
    .collect())
}

/// One edit by ID.
pub async fn get(exec: impl PgExecutor<'_>, id: uuid::Uuid) -> Result<Option<Brief>, DbError> {
    Ok(
        sqlx::query!("SELECT * FROM change_briefs WHERE id = $1", id)
            .fetch_optional(exec)
            .await?
            .map(|r| brief!(r)),
    )
}

/// Every edit of a merge request's briefs, oldest first.
pub async fn for_mr(exec: impl PgExecutor<'_>, mr: MergeRequestId) -> Result<Vec<Brief>, DbError> {
    Ok(sqlx::query!(
        "SELECT * FROM change_briefs WHERE mr_id = $1 ORDER BY created_at, id",
        mr.as_uuid()
    )
    .fetch_all(exec)
    .await?
    .into_iter()
    .map(|r| brief!(r))
    .collect())
}
