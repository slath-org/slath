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

/// A merge request as pages and the API show it.
#[derive(Debug, Clone)]
pub struct MergeRequest {
    pub id: MergeRequestId,
    pub repo_id: RepoId,
    pub number: i32,
    pub author_id: Option<slath_core::ids::UserId>,
    /// `None` for deleted accounts.
    pub author: Option<String>,
    pub title: String,
    pub description: String,
    pub draft: bool,
    pub target_branch: String,
    pub source_repo_id: Option<RepoId>,
    /// Full ref name in the source repository.
    pub source_ref: Vec<u8>,
    pub allow_maintainer_edits: bool,
    /// `open`, `merged`, or `closed`.
    pub state: String,
    pub milestone_id: Option<uuid::Uuid>,
    pub delete_source_on_merge: bool,
    pub merged_at: Option<time::OffsetDateTime>,
    pub closed_at: Option<time::OffsetDateTime>,
    pub created_at: time::OffsetDateTime,
    pub updated_at: time::OffsetDateTime,
    pub comments: i64,
    /// Head of the current (highest) revision, if any.
    pub head_oid: Option<String>,
    pub revisions: i32,
    /// Fork MRs: what push protection found in a source head that wasn't
    /// fetched (see `service::push_protection::BlockedSource`).
    pub source_findings: Option<serde_json::Value>,
}

impl MergeRequest {
    /// The source branch's short name (lossy for non-UTF-8 names).
    #[must_use]
    pub fn source_branch(&self) -> String {
        let r = String::from_utf8_lossy(&self.source_ref);
        r.strip_prefix("refs/heads/").unwrap_or(&r).to_owned()
    }
}

macro_rules! to_mr {
    ($r:expr) => {
        MergeRequest {
            id: MergeRequestId::from_uuid($r.id),
            repo_id: RepoId::from_uuid($r.repo_id),
            number: $r.number,
            author_id: $r.author_id.map(slath_core::ids::UserId::from_uuid),
            author: $r.author,
            title: $r.title,
            description: $r.description,
            draft: $r.draft,
            target_branch: $r.target_branch,
            source_repo_id: $r.source_repo_id.map(RepoId::from_uuid),
            source_ref: $r.source_ref,
            allow_maintainer_edits: $r.allow_maintainer_edits,
            state: $r.state,
            milestone_id: $r.milestone_id,
            delete_source_on_merge: $r.delete_source_on_merge,
            merged_at: $r.merged_at,
            closed_at: $r.closed_at,
            created_at: $r.created_at,
            updated_at: $r.updated_at,
            comments: $r.comments,
            head_oid: $r.head_oid,
            revisions: $r.revisions,
            source_findings: $r.source_findings,
        }
    };
}

/// What a new merge request needs.
#[derive(Debug)]
pub struct NewMergeRequest<'a> {
    pub repo: RepoId,
    pub number: i32,
    pub author: slath_core::ids::UserId,
    pub title: &'a str,
    pub description: &'a str,
    pub draft: bool,
    pub target_branch: &'a str,
    pub source_repo: RepoId,
    pub source_ref: &'a [u8],
    pub allow_maintainer_edits: bool,
    pub delete_source_on_merge: bool,
    pub milestone: Option<uuid::Uuid>,
}

/// Inserts a merge request. A second open one for the same source and
/// target is a [`DbError::Conflict`] on `source`.
pub async fn insert(
    tx: &mut sqlx::PgConnection,
    m: &NewMergeRequest<'_>,
) -> Result<MergeRequestId, DbError> {
    let id = MergeRequestId::new();
    let r = sqlx::query!(
        "INSERT INTO merge_requests (id, repo_id, number, author_id, title, description, draft,
             target_branch, source_repo_id, source_ref, allow_maintainer_edits,
             delete_source_on_merge, milestone_id)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13)",
        id.as_uuid(),
        m.repo.as_uuid(),
        m.number,
        m.author.as_uuid(),
        m.title,
        m.description,
        m.draft,
        m.target_branch,
        m.source_repo.as_uuid(),
        m.source_ref,
        m.allow_maintainer_edits,
        m.delete_source_on_merge,
        m.milestone
    )
    .execute(&mut *tx)
    .await;
    match r {
        Ok(_) => Ok(id),
        Err(sqlx::Error::Database(e)) if e.constraint() == Some("merge_requests_open_pair") => {
            Err(DbError::Conflict("source"))
        }
        Err(e) => Err(e.into()),
    }
}

/// One merge request by number.
pub async fn get(
    exec: impl PgExecutor<'_>,
    repo: RepoId,
    number: i32,
) -> Result<Option<MergeRequest>, DbError> {
    let r = sqlx::query!(
        r#"SELECT m.id, m.repo_id, m.number, m.author_id, n.name::text AS author, m.title, m.description,
                  m.draft, m.target_branch, m.source_repo_id, m.source_ref, m.allow_maintainer_edits,
                  m.state, m.milestone_id, m.delete_source_on_merge, m.merged_at, m.closed_at, m.source_findings,
                  m.created_at, m.updated_at,
                  (SELECT count(*) FROM comments c WHERE c.subject_kind = 'merge_request' AND c.subject_id = m.id
                     AND c.deleted_at IS NULL) AS "comments!",
                  (SELECT v.head_oid FROM mr_revisions v WHERE v.mr_id = m.id ORDER BY v.seq DESC LIMIT 1) AS head_oid,
                  (SELECT count(*)::int FROM mr_revisions v WHERE v.mr_id = m.id) AS "revisions!"
           FROM merge_requests m LEFT JOIN users u ON u.id = m.author_id AND u.state <> 'deleted'
           LEFT JOIN namespaces n ON n.id = u.namespace_id
           WHERE m.repo_id = $1 AND m.number = $2"#,
        repo.as_uuid(),
        number
    )
    .fetch_optional(exec)
    .await?;
    Ok(r.map(|r| to_mr!(r)))
}

/// One merge request by ID.
pub async fn get_by_id(
    exec: impl PgExecutor<'_>,
    id: MergeRequestId,
) -> Result<Option<MergeRequest>, DbError> {
    let r = sqlx::query!(
        r#"SELECT m.id, m.repo_id, m.number, m.author_id, n.name::text AS author, m.title, m.description,
                  m.draft, m.target_branch, m.source_repo_id, m.source_ref, m.allow_maintainer_edits,
                  m.state, m.milestone_id, m.delete_source_on_merge, m.merged_at, m.closed_at, m.source_findings,
                  m.created_at, m.updated_at,
                  (SELECT count(*) FROM comments c WHERE c.subject_kind = 'merge_request' AND c.subject_id = m.id
                     AND c.deleted_at IS NULL) AS "comments!",
                  (SELECT v.head_oid FROM mr_revisions v WHERE v.mr_id = m.id ORDER BY v.seq DESC LIMIT 1) AS head_oid,
                  (SELECT count(*)::int FROM mr_revisions v WHERE v.mr_id = m.id) AS "revisions!"
           FROM merge_requests m LEFT JOIN users u ON u.id = m.author_id AND u.state <> 'deleted'
           LEFT JOIN namespaces n ON n.id = u.namespace_id
           WHERE m.id = $1"#,
        id.as_uuid()
    )
    .fetch_optional(exec)
    .await?;
    Ok(r.map(|r| to_mr!(r)))
}

/// Merge requests of a repository matching an issue-style filter (state
/// `open`, `merged`, `closed`, or all).
pub async fn list(
    exec: impl PgExecutor<'_>,
    repo: RepoId,
    f: &crate::issues::Filter,
    offset: i64,
    limit: i64,
) -> Result<Vec<MergeRequest>, DbError> {
    let rows = sqlx::query!(
        r#"SELECT m.id, m.repo_id, m.number, m.author_id, n.name::text AS author, m.title, m.description,
                  m.draft, m.target_branch, m.source_repo_id, m.source_ref, m.allow_maintainer_edits,
                  m.state, m.milestone_id, m.delete_source_on_merge, m.merged_at, m.closed_at, m.source_findings,
                  m.created_at, m.updated_at,
                  (SELECT count(*) FROM comments c WHERE c.subject_kind = 'merge_request' AND c.subject_id = m.id
                     AND c.deleted_at IS NULL) AS "comments!",
                  (SELECT v.head_oid FROM mr_revisions v WHERE v.mr_id = m.id ORDER BY v.seq DESC LIMIT 1) AS head_oid,
                  (SELECT count(*)::int FROM mr_revisions v WHERE v.mr_id = m.id) AS "revisions!"
           FROM merge_requests m LEFT JOIN users u ON u.id = m.author_id AND u.state <> 'deleted'
           LEFT JOIN namespaces n ON n.id = u.namespace_id
           WHERE m.repo_id = $1
             AND ($2::text IS NULL OR m.state = $2)
             AND ($3::uuid IS NULL OR m.author_id = $3)
             AND ($4::uuid IS NULL OR EXISTS (SELECT 1 FROM mr_assignees a WHERE a.mr_id = m.id AND a.user_id = $4))
             AND (SELECT count(*) FROM mr_labels l WHERE l.mr_id = m.id AND l.label_id = ANY($5)) = cardinality($5)
             AND ($6::uuid IS NULL OR m.milestone_id = $6)
           ORDER BY CASE WHEN $7 THEN m.updated_at ELSE m.created_at END DESC, m.number DESC
           OFFSET $8 LIMIT $9"#,
        repo.as_uuid(),
        f.state,
        f.author.map(|u| *u.as_uuid()),
        f.assignee.map(|u| *u.as_uuid()),
        &f.labels,
        f.milestone,
        f.sort_updated,
        offset,
        limit
    )
    .fetch_all(exec)
    .await?;
    Ok(rows.into_iter().map(|r| to_mr!(r)).collect())
}

/// `(open, merged, closed)` counts matching a filter (its state ignored).
pub async fn counts(
    exec: impl PgExecutor<'_>,
    repo: RepoId,
    f: &crate::issues::Filter,
) -> Result<(i64, i64, i64), DbError> {
    let r = sqlx::query!(
        r#"SELECT count(*) FILTER (WHERE m.state = 'open') AS "open!",
                  count(*) FILTER (WHERE m.state = 'merged') AS "merged!",
                  count(*) FILTER (WHERE m.state = 'closed') AS "closed!"
           FROM merge_requests m WHERE m.repo_id = $1
             AND ($2::uuid IS NULL OR m.author_id = $2)
             AND ($3::uuid IS NULL OR EXISTS (SELECT 1 FROM mr_assignees a WHERE a.mr_id = m.id AND a.user_id = $3))
             AND (SELECT count(*) FROM mr_labels l WHERE l.mr_id = m.id AND l.label_id = ANY($4)) = cardinality($4)
             AND ($5::uuid IS NULL OR m.milestone_id = $5)"#,
        repo.as_uuid(),
        f.author.map(|u| *u.as_uuid()),
        f.assignee.map(|u| *u.as_uuid()),
        &f.labels,
        f.milestone
    )
    .fetch_one(exec)
    .await?;
    Ok((r.open, r.merged, r.closed))
}

/// Open merge requests whose source is `(repo, ref)`.
pub async fn open_by_source(
    exec: impl PgExecutor<'_>,
    repo: RepoId,
    source_ref: &[u8],
) -> Result<Vec<(MergeRequestId, RepoId, i32)>, DbError> {
    let rows = sqlx::query!(
        "SELECT id, repo_id, number FROM merge_requests
          WHERE source_repo_id = $1 AND source_ref = $2 AND state = 'open' ORDER BY number",
        repo.as_uuid(),
        source_ref
    )
    .fetch_all(exec)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| {
            (
                MergeRequestId::from_uuid(r.id),
                RepoId::from_uuid(r.repo_id),
                r.number,
            )
        })
        .collect())
}

pub async fn update_text(
    exec: impl PgExecutor<'_>,
    id: MergeRequestId,
    title: &str,
    description: &str,
) -> Result<(), DbError> {
    sqlx::query!(
        "UPDATE merge_requests SET title = $2, description = $3, updated_at = now() WHERE id = $1",
        id.as_uuid(),
        title,
        description
    )
    .execute(exec)
    .await?;
    Ok(())
}

/// Sets draft; returns false if it already had that value.
pub async fn set_draft(
    exec: impl PgExecutor<'_>,
    id: MergeRequestId,
    draft: bool,
) -> Result<bool, DbError> {
    Ok(sqlx::query!(
        "UPDATE merge_requests SET draft = $2, updated_at = now() WHERE id = $1 AND draft <> $2",
        id.as_uuid(),
        draft
    )
    .execute(exec)
    .await?
    .rows_affected()
        == 1)
}

/// Closes or reopens an open/closed merge request; false if unchanged.
/// A second open merge request for the same pair makes reopening a
/// [`DbError::Conflict`].
pub async fn set_open(
    exec: impl PgExecutor<'_>,
    id: MergeRequestId,
    open: bool,
) -> Result<bool, DbError> {
    let r = sqlx::query!(
        "UPDATE merge_requests SET state = CASE WHEN $2 THEN 'open' ELSE 'closed' END,
             closed_at = CASE WHEN $2 THEN NULL ELSE now() END, updated_at = now()
         WHERE id = $1 AND state = CASE WHEN $2 THEN 'closed' ELSE 'open' END",
        id.as_uuid(),
        open
    )
    .execute(exec)
    .await;
    match r {
        Ok(r) => Ok(r.rows_affected() == 1),
        Err(sqlx::Error::Database(e)) if e.constraint() == Some("merge_requests_open_pair") => {
            Err(DbError::Conflict("source"))
        }
        Err(e) => Err(e.into()),
    }
}

pub async fn touch(exec: impl PgExecutor<'_>, id: MergeRequestId) -> Result<(), DbError> {
    sqlx::query!(
        "UPDATE merge_requests SET updated_at = now() WHERE id = $1",
        id.as_uuid()
    )
    .execute(exec)
    .await?;
    Ok(())
}

/// A revision of a merge request.
#[derive(Debug, Clone)]
pub struct Revision {
    pub id: uuid::Uuid,
    pub seq: i32,
    pub base_oid: String,
    pub merge_base_oid: Option<String>,
    pub head_oid: String,
    pub pushed_by: Option<String>,
    pub commit_count: i32,
    pub mode: String,
    pub mode_reason: Option<String>,
    pub created_at: time::OffsetDateTime,
}

/// What a new revision records.
#[derive(Debug)]
pub struct NewRevision<'a> {
    pub mr: MergeRequestId,
    pub seq: i32,
    pub base_oid: &'a str,
    pub merge_base_oid: Option<&'a str>,
    pub head_oid: &'a str,
    pub pushed_by: Option<slath_core::ids::UserId>,
    pub commit_count: i32,
    pub mode: &'a str,
    pub mode_reason: Option<&'a str>,
}

pub async fn insert_revision(
    exec: impl PgExecutor<'_>,
    r: &NewRevision<'_>,
) -> Result<uuid::Uuid, DbError> {
    let id = uuid::Uuid::now_v7();
    sqlx::query!(
        "INSERT INTO mr_revisions (id, mr_id, seq, base_oid, merge_base_oid, head_oid, pushed_by,
             commit_count, mode, mode_reason)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)",
        id,
        r.mr.as_uuid(),
        r.seq,
        r.base_oid,
        r.merge_base_oid,
        r.head_oid,
        r.pushed_by.map(|u| *u.as_uuid()),
        r.commit_count,
        r.mode,
        r.mode_reason
    )
    .execute(exec)
    .await?;
    Ok(id)
}

/// Every revision, oldest first.
pub async fn revisions(
    exec: impl PgExecutor<'_>,
    mr: MergeRequestId,
) -> Result<Vec<Revision>, DbError> {
    let rows = sqlx::query!(
        r#"SELECT v.id, v.seq, v.base_oid, v.merge_base_oid, v.head_oid, n.name::text AS pushed_by,
                  v.commit_count, v.mode, v.mode_reason, v.created_at
           FROM mr_revisions v LEFT JOIN users u ON u.id = v.pushed_by AND u.state <> 'deleted'
           LEFT JOIN namespaces n ON n.id = u.namespace_id
           WHERE v.mr_id = $1 ORDER BY v.seq"#,
        mr.as_uuid()
    )
    .fetch_all(exec)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| Revision {
            id: r.id,
            seq: r.seq,
            base_oid: r.base_oid,
            merge_base_oid: r.merge_base_oid,
            head_oid: r.head_oid,
            pushed_by: r.pushed_by,
            commit_count: r.commit_count,
            mode: r.mode,
            mode_reason: r.mode_reason,
            created_at: r.created_at,
        })
        .collect())
}

/// Users assigned to a merge request, by name.
pub async fn assignees(
    exec: impl PgExecutor<'_>,
    mr: MergeRequestId,
) -> Result<Vec<String>, DbError> {
    Ok(sqlx::query_scalar!(
        r#"SELECT n.name::text AS "name!" FROM mr_assignees a JOIN users u ON u.id = a.user_id
           JOIN namespaces n ON n.id = u.namespace_id WHERE a.mr_id = $1 ORDER BY 1"#,
        mr.as_uuid()
    )
    .fetch_all(exec)
    .await?)
}

pub async fn add_assignee(
    exec: impl PgExecutor<'_>,
    mr: MergeRequestId,
    user: slath_core::ids::UserId,
) -> Result<(), DbError> {
    sqlx::query!(
        "INSERT INTO mr_assignees (mr_id, user_id) VALUES ($1, $2) ON CONFLICT DO NOTHING",
        mr.as_uuid(),
        user.as_uuid()
    )
    .execute(exec)
    .await?;
    Ok(())
}

pub async fn add_reviewer(
    exec: impl PgExecutor<'_>,
    mr: MergeRequestId,
    user: slath_core::ids::UserId,
    by: slath_core::ids::UserId,
) -> Result<(), DbError> {
    sqlx::query!(
        "INSERT INTO mr_reviewers (id, mr_id, user_id, requested_by) VALUES ($1, $2, $3, $4)
         ON CONFLICT (mr_id, user_id) WHERE user_id IS NOT NULL DO UPDATE SET state = 'requested'",
        uuid::Uuid::now_v7(),
        mr.as_uuid(),
        user.as_uuid(),
        by.as_uuid()
    )
    .execute(exec)
    .await?;
    Ok(())
}

/// Labels of a merge request.
pub async fn labels(
    exec: impl PgExecutor<'_>,
    mr: MergeRequestId,
) -> Result<Vec<crate::labels::Label>, DbError> {
    let rows = sqlx::query!(
        "SELECT l.id, l.name::text AS \"name!\", l.color, l.description FROM mr_labels m
           JOIN labels l ON l.id = m.label_id WHERE m.mr_id = $1 ORDER BY l.name",
        mr.as_uuid()
    )
    .fetch_all(exec)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| crate::labels::Label {
            id: r.id,
            name: r.name,
            color: r.color,
            description: r.description,
            open_issues: 0,
        })
        .collect())
}

pub async fn add_label(
    exec: impl PgExecutor<'_>,
    mr: MergeRequestId,
    label: uuid::Uuid,
) -> Result<(), DbError> {
    sqlx::query!(
        "INSERT INTO mr_labels (mr_id, label_id) VALUES ($1, $2) ON CONFLICT DO NOTHING",
        mr.as_uuid(),
        label
    )
    .execute(exec)
    .await?;
    Ok(())
}

/// `(number, title, state)` of merge requests by number (reference
/// rendering).
pub async fn titles(
    exec: impl PgExecutor<'_>,
    repo: RepoId,
    numbers: &[i32],
) -> Result<Vec<(i32, String, String)>, DbError> {
    let rows = sqlx::query!(
        "SELECT number, title, state FROM merge_requests WHERE repo_id = $1 AND number = ANY($2)",
        repo.as_uuid(),
        numbers
    )
    .fetch_all(exec)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| (r.number, r.title, r.state))
        .collect())
}

/// Marks a merge request merged.
pub async fn set_merged(
    exec: impl PgExecutor<'_>,
    mr: MergeRequestId,
    by: Option<slath_core::ids::UserId>,
    strategy: &str,
    merged_oid: &str,
) -> Result<bool, DbError> {
    let done = sqlx::query!(
        "UPDATE merge_requests SET state = 'merged', merged_at = now(), merged_by = $2,
             merge_strategy = $3, merged_oid = $4, updated_at = now()
         WHERE id = $1 AND state = 'open'",
        mr.as_uuid(),
        by.map(|u| *u.as_uuid()),
        strategy,
        merged_oid
    )
    .execute(exec)
    .await?;
    Ok(done.rows_affected() == 1)
}

/// Holds the merge lock of a target branch until the transaction ends.
pub async fn lock_target(
    exec: impl PgExecutor<'_>,
    repo: RepoId,
    target: &str,
) -> Result<(), DbError> {
    sqlx::query!(
        "SELECT pg_advisory_xact_lock(hashtextextended($1, 0))",
        format!("merge:{}:{target}", repo.as_uuid())
    )
    .execute(exec)
    .await?;
    Ok(())
}

/// Open merge requests that target `branch` of `repo`.
pub async fn open_by_target(
    exec: impl PgExecutor<'_>,
    repo: RepoId,
    branch: &str,
) -> Result<Vec<MergeRequestId>, DbError> {
    let rows = sqlx::query_scalar!(
        "SELECT id FROM merge_requests WHERE repo_id = $1 AND target_branch = $2 AND state = 'open'",
        repo.as_uuid(),
        branch
    )
    .fetch_all(exec)
    .await?;
    Ok(rows.into_iter().map(MergeRequestId::from_uuid).collect())
}

/// Records (or, with `None`, clears) what push protection found in a fork
/// MR's source head.
pub async fn set_source_findings(
    exec: impl PgExecutor<'_>,
    id: MergeRequestId,
    findings: Option<&serde_json::Value>,
) -> Result<(), DbError> {
    sqlx::query!(
        "UPDATE merge_requests SET source_findings = $2 WHERE id = $1",
        id.as_uuid(),
        findings
    )
    .execute(exec)
    .await?;
    Ok(())
}

/// Open merge requests into `repo` whose source head push protection held
/// back.
pub async fn held_back(
    exec: impl PgExecutor<'_>,
    repo: RepoId,
) -> Result<Vec<MergeRequestId>, DbError> {
    Ok(sqlx::query_scalar!(
        "SELECT id FROM merge_requests
          WHERE repo_id = $1 AND state = 'open' AND source_findings IS NOT NULL",
        repo.as_uuid()
    )
    .fetch_all(exec)
    .await?
    .into_iter()
    .map(MergeRequestId::from_uuid)
    .collect())
}

/// An open merge request from a repository into another one that allows
/// edits by maintainers.
#[derive(Debug, Clone)]
pub struct EditableSource {
    pub id: MergeRequestId,
    pub target: RepoId,
    pub source_ref: Vec<u8>,
    pub author: Option<slath_core::ids::UserId>,
}

/// Open merge requests from `source` into other repositories that allow
/// edits by maintainers.
pub async fn maintainer_editable(
    exec: impl PgExecutor<'_>,
    source: RepoId,
) -> Result<Vec<EditableSource>, DbError> {
    let rows = sqlx::query!(
        "SELECT id, repo_id, source_ref, author_id FROM merge_requests
          WHERE source_repo_id = $1 AND repo_id <> $1 AND state = 'open'
            AND allow_maintainer_edits ORDER BY number",
        source.as_uuid()
    )
    .fetch_all(exec)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| EditableSource {
            id: MergeRequestId::from_uuid(r.id),
            target: RepoId::from_uuid(r.repo_id),
            source_ref: r.source_ref,
            author: r.author_id.map(slath_core::ids::UserId::from_uuid),
        })
        .collect())
}

/// Sets whether a merge request allows edits by maintainers.
pub async fn set_allow_maintainer_edits(
    exec: impl PgExecutor<'_>,
    id: MergeRequestId,
    allow: bool,
) -> Result<bool, DbError> {
    Ok(sqlx::query!(
        "UPDATE merge_requests SET allow_maintainer_edits = $2, updated_at = now()
          WHERE id = $1 AND allow_maintainer_edits <> $2",
        id.as_uuid(),
        allow
    )
    .execute(exec)
    .await?
    .rows_affected()
        > 0)
}
