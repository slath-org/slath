// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Finding threads: review threads made from a commit status's line
//! annotations, one per finding, context, and revision.

use slath_core::ids::{MergeRequestId, RepoId, UserId};
use sqlx::PgExecutor;

use crate::DbError;

/// What makes a thread a finding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    pub context: String,
    /// `notice`, `warning`, or `failure`.
    pub severity: String,
    pub category: String,
}

/// A revision of an open merge request whose head is a given commit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub mr: MergeRequestId,
    pub seq: i32,
    pub merge_base_oid: Option<String>,
}

/// A finding thread of one context on a revision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Existing {
    pub thread: uuid::Uuid,
    pub key: String,
    pub path: Option<String>,
    pub resolved: bool,
    /// Resolved because a later status stopped reporting it.
    pub resolved_by_status: bool,
}

/// The revisions of open merge requests of `repo` whose head is `oid`.
pub async fn targets(
    exec: impl PgExecutor<'_>,
    repo: RepoId,
    oid: &str,
) -> Result<Vec<Target>, DbError> {
    let rows = sqlx::query!(
        "SELECT v.mr_id, v.seq, v.merge_base_oid
         FROM mr_revisions v JOIN merge_requests m ON m.id = v.mr_id
         WHERE m.repo_id = $1 AND m.state = 'open' AND v.head_oid = $2
         ORDER BY m.number, v.seq",
        repo.as_uuid(),
        oid
    )
    .fetch_all(exec)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| Target {
            mr: MergeRequestId::from_uuid(r.mr_id),
            seq: r.seq,
            merge_base_oid: r.merge_base_oid,
        })
        .collect())
}

/// Serializes finding updates of one merge request until the transaction
/// ends.
pub async fn lock(exec: impl PgExecutor<'_>, mr: MergeRequestId) -> Result<(), DbError> {
    sqlx::query!(
        "SELECT pg_advisory_xact_lock(hashtext('findings:' || $1::uuid::text))",
        mr.as_uuid()
    )
    .fetch_one(exec)
    .await?;
    Ok(())
}

/// The finding threads of `context` on revision `seq`.
pub async fn of_context(
    exec: impl PgExecutor<'_>,
    mr: MergeRequestId,
    seq: i32,
    context: &str,
) -> Result<Vec<Existing>, DbError> {
    let rows = sqlx::query!(
        r#"SELECT f.thread_id, f.key, t.path, t.resolved_at IS NOT NULL AS "resolved!",
                  f.resolved_by_status IS NOT NULL AS "by_status!"
           FROM mr_thread_findings f JOIN mr_threads t ON t.id = f.thread_id
           WHERE f.mr_id = $1 AND f.revision_seq = $2 AND f.context = $3"#,
        mr.as_uuid(),
        seq,
        context
    )
    .fetch_all(exec)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| Existing {
            thread: r.thread_id,
            key: r.key,
            path: r.path,
            resolved: r.resolved,
            resolved_by_status: r.by_status,
        })
        .collect())
}

/// What a new finding thread records besides the thread itself.
#[derive(Debug)]
pub struct NewFinding<'a> {
    pub thread: uuid::Uuid,
    pub mr: MergeRequestId,
    pub seq: i32,
    pub context: &'a str,
    pub key: &'a str,
    pub severity: &'a str,
    pub category: &'a str,
    pub status: uuid::Uuid,
}

pub async fn insert(exec: impl PgExecutor<'_>, f: &NewFinding<'_>) -> Result<(), DbError> {
    sqlx::query!(
        "INSERT INTO mr_thread_findings (thread_id, mr_id, revision_seq, context, key, severity,
             category, status_id)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
        f.thread,
        f.mr.as_uuid(),
        f.seq,
        f.context,
        f.key,
        f.severity,
        f.category,
        f.status
    )
    .execute(exec)
    .await?;
    Ok(())
}

/// Resolves a finding thread for a status that no longer reports it
/// (`Some`), or reopens it (`None`).
pub async fn set_resolved_by_status(
    exec: impl PgExecutor<'_>,
    thread: uuid::Uuid,
    by: Option<(uuid::Uuid, UserId)>,
) -> Result<(), DbError> {
    sqlx::query!(
        "WITH f AS (UPDATE mr_thread_findings SET resolved_by_status = $2 WHERE thread_id = $1)
         UPDATE mr_threads SET resolved_at = CASE WHEN $2::uuid IS NULL THEN NULL ELSE now() END,
             resolved_by = $3
         WHERE id = $1",
        thread,
        by.map(|(s, _)| s),
        by.map(|(_, u)| *u.as_uuid())
    )
    .execute(exec)
    .await?;
    Ok(())
}
