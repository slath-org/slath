// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Merge requests and their revisions.

use slath_core::ids::{MergeRequestId, RepoId};
use sqlx::PgExecutor;

use crate::DbError;

/// A revision's pinned head, as the reconciler checks it.
#[derive(Debug, Clone)]
pub struct RevisionHead {
    pub mr_id: MergeRequestId,
    pub seq: i32,
    pub head_oid: String,
    pub missing_objects: bool,
}

/// Every revision head of the merge requests targeting `repo`, oldest first.
pub async fn revision_heads(
    db: impl PgExecutor<'_>,
    repo: RepoId,
) -> Result<Vec<RevisionHead>, DbError> {
    let rows = sqlx::query!(
        "SELECT v.mr_id, v.seq, v.head_oid, v.missing_objects
           FROM mr_revisions v JOIN merge_requests m ON m.id = v.mr_id
          WHERE m.repo_id = $1
          ORDER BY v.mr_id, v.seq",
        repo.as_uuid()
    )
    .fetch_all(db)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| RevisionHead {
            mr_id: MergeRequestId::from_uuid(r.mr_id),
            seq: r.seq,
            head_oid: r.head_oid,
            missing_objects: r.missing_objects,
        })
        .collect())
}

/// Records whether a revision's pinned head is missing from Git.
pub async fn set_revision_missing(
    db: impl PgExecutor<'_>,
    mr: MergeRequestId,
    seq: i32,
    missing: bool,
) -> Result<(), DbError> {
    sqlx::query!(
        "UPDATE mr_revisions SET missing_objects = $3 WHERE mr_id = $1 AND seq = $2",
        mr.as_uuid(),
        seq,
        missing
    )
    .execute(db)
    .await?;
    Ok(())
}
