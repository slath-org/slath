// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Reviews of merge requests and review requests.

use slath_core::ids::{MergeRequestId, RepoId, UserId};
use sqlx::PgExecutor;

use crate::DbError;

/// A submitted review.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Review {
    pub id: uuid::Uuid,
    pub revision_seq: i32,
    pub reviewer_id: Option<UserId>,
    /// `None` once the account is deleted.
    pub reviewer: Option<String>,
    /// `approve`, `request_changes`, or `comment`.
    pub verdict: String,
    pub body: String,
    pub created_at: time::OffsetDateTime,
}

pub async fn insert(
    exec: impl PgExecutor<'_>,
    mr: MergeRequestId,
    revision_seq: i32,
    reviewer: UserId,
    verdict: &str,
    body: &str,
) -> Result<uuid::Uuid, DbError> {
    let id = uuid::Uuid::now_v7();
    sqlx::query!(
        "INSERT INTO mr_reviews (id, mr_id, revision_seq, reviewer_id, verdict, body)
         VALUES ($1, $2, $3, $4, $5, $6)",
        id,
        mr.as_uuid(),
        revision_seq,
        reviewer.as_uuid(),
        verdict,
        body
    )
    .execute(exec)
    .await?;
    Ok(id)
}

/// Records an approval's coverage: the files in the approved revision's
/// diff, and the ones the approver had viewed.
pub async fn set_coverage(
    conn: &mut sqlx::PgConnection,
    review: uuid::Uuid,
    files_total: i32,
    viewed: &[String],
) -> Result<(), DbError> {
    sqlx::query!(
        "UPDATE mr_reviews SET files_total = $2 WHERE id = $1",
        review,
        files_total
    )
    .execute(&mut *conn)
    .await?;
    sqlx::query!(
        "INSERT INTO mr_review_viewed_files (review_id, path)
         SELECT $1, p FROM unnest($2::text[]) AS p",
        review,
        viewed
    )
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// An approval's coverage: the files in its revision's diff, and the
/// paths the approver had viewed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Coverage {
    pub files_total: i32,
    pub viewed: Vec<String>,
}

/// The coverage of each of `reviews` that has one.
pub async fn coverage(
    exec: impl PgExecutor<'_>,
    reviews: &[uuid::Uuid],
) -> Result<std::collections::HashMap<uuid::Uuid, Coverage>, DbError> {
    let rows = sqlx::query!(
        r#"SELECT r.id, r.files_total AS "files_total!",
                  COALESCE(array_agg(v.path ORDER BY v.path) FILTER (WHERE v.path IS NOT NULL),
                           '{}') AS "viewed!"
           FROM mr_reviews r
           LEFT JOIN mr_review_viewed_files v ON v.review_id = r.id
           WHERE r.id = ANY($1) AND r.files_total IS NOT NULL
           GROUP BY r.id, r.files_total"#,
        reviews
    )
    .fetch_all(exec)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| {
            (
                r.id,
                Coverage {
                    files_total: r.files_total,
                    viewed: r.viewed,
                },
            )
        })
        .collect())
}

/// Every review, oldest first.
pub async fn list(exec: impl PgExecutor<'_>, mr: MergeRequestId) -> Result<Vec<Review>, DbError> {
    let rows = sqlx::query!(
        r#"SELECT r.id, r.revision_seq, r.reviewer_id, n.name::text AS reviewer, r.verdict, r.body,
                  r.created_at
           FROM mr_reviews r
           LEFT JOIN users u ON u.id = r.reviewer_id AND u.state <> 'deleted'
           LEFT JOIN namespaces n ON n.id = u.namespace_id
           WHERE r.mr_id = $1 ORDER BY r.created_at, r.id"#,
        mr.as_uuid()
    )
    .fetch_all(exec)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| Review {
            id: r.id,
            revision_seq: r.revision_seq,
            reviewer_id: r.reviewer_id.map(UserId::from_uuid),
            reviewer: r.reviewer,
            verdict: r.verdict,
            body: r.body,
            created_at: r.created_at,
        })
        .collect())
}

/// A requested reviewer: a user or a team.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Requested {
    pub user: Option<UserId>,
    pub team: Option<uuid::Uuid>,
    /// The user's name, or `org/team`.
    pub name: String,
    /// `requested` or `reviewed`.
    pub state: String,
}

/// The current review requests (not removed).
pub async fn requested(
    exec: impl PgExecutor<'_>,
    mr: MergeRequestId,
) -> Result<Vec<Requested>, DbError> {
    let rows = sqlx::query!(
        r#"SELECT r.user_id, r.team_id, r.state,
                  COALESCE(un.name::text, tn.name::text || '/' || t.name::text) AS "name!"
           FROM mr_reviewers r
           LEFT JOIN users u ON u.id = r.user_id
           LEFT JOIN namespaces un ON un.id = u.namespace_id
           LEFT JOIN teams t ON t.id = r.team_id
           LEFT JOIN organizations o ON o.id = t.org_id
           LEFT JOIN namespaces tn ON tn.id = o.namespace_id
           WHERE r.mr_id = $1 AND r.state <> 'removed'
             AND (un.name IS NOT NULL OR tn.name IS NOT NULL)
           ORDER BY 4"#,
        mr.as_uuid()
    )
    .fetch_all(exec)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| Requested {
            user: r.user_id.map(UserId::from_uuid),
            team: r.team_id,
            name: r.name,
            state: r.state,
        })
        .collect())
}

pub async fn request_team(
    exec: impl PgExecutor<'_>,
    mr: MergeRequestId,
    team: uuid::Uuid,
    by: UserId,
) -> Result<(), DbError> {
    sqlx::query!(
        "INSERT INTO mr_reviewers (id, mr_id, team_id, requested_by) VALUES ($1, $2, $3, $4)
         ON CONFLICT (mr_id, team_id) WHERE team_id IS NOT NULL DO UPDATE SET state = 'requested'",
        uuid::Uuid::now_v7(),
        mr.as_uuid(),
        team,
        by.as_uuid()
    )
    .execute(exec)
    .await?;
    Ok(())
}

/// Withdraws the request of a user or a team.
pub async fn remove_request(
    exec: impl PgExecutor<'_>,
    mr: MergeRequestId,
    user: Option<UserId>,
    team: Option<uuid::Uuid>,
) -> Result<(), DbError> {
    sqlx::query!(
        "UPDATE mr_reviewers SET state = 'removed'
         WHERE mr_id = $1 AND (user_id = $2 OR team_id = $3)",
        mr.as_uuid(),
        user.map(|u| *u.as_uuid()),
        team
    )
    .execute(exec)
    .await?;
    Ok(())
}

/// A user reviewed: their request, and their teams' requests, are done.
pub async fn mark_reviewed(
    exec: impl PgExecutor<'_>,
    mr: MergeRequestId,
    user: UserId,
) -> Result<(), DbError> {
    sqlx::query!(
        "UPDATE mr_reviewers SET state = 'reviewed'
         WHERE mr_id = $1 AND state = 'requested'
           AND (user_id = $2 OR team_id IN (SELECT team_id FROM team_members WHERE user_id = $2))",
        mr.as_uuid(),
        user.as_uuid()
    )
    .execute(exec)
    .await?;
    Ok(())
}

/// An open merge request waiting for the user's review.
#[derive(Debug, Clone)]
pub struct Waiting {
    pub mr: MergeRequestId,
    pub repo: RepoId,
    pub number: i32,
    pub title: String,
    pub namespace: String,
    pub repo_name: String,
    pub author: Option<String>,
    pub updated_at: time::OffsetDateTime,
}

/// Open, ready merge requests that request a review from `user`, directly
/// or through a team, and that the user didn't author.
pub async fn waiting_for(
    exec: impl PgExecutor<'_>,
    user: UserId,
    limit: i64,
) -> Result<Vec<Waiting>, DbError> {
    let rows = sqlx::query!(
        r#"SELECT DISTINCT m.id, m.repo_id, m.number, m.title, n.name::text AS "namespace!",
                  p.name::text AS "repo_name!", an.name::text AS author, m.updated_at
           FROM mr_reviewers r
           JOIN merge_requests m ON m.id = r.mr_id
           JOIN repositories p ON p.id = m.repo_id AND p.deleted_at IS NULL
           JOIN namespaces n ON n.id = p.namespace_id
           LEFT JOIN users a ON a.id = m.author_id
           LEFT JOIN namespaces an ON an.id = a.namespace_id
           WHERE r.state = 'requested' AND m.state = 'open' AND NOT m.draft
             AND m.author_id IS DISTINCT FROM $1
             AND (r.user_id = $1 OR r.team_id IN (SELECT team_id FROM team_members WHERE user_id = $1))
           ORDER BY m.updated_at DESC
           LIMIT $2"#,
        user.as_uuid(),
        limit
    )
    .fetch_all(exec)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| Waiting {
            mr: MergeRequestId::from_uuid(r.id),
            repo: RepoId::from_uuid(r.repo_id),
            number: r.number,
            title: r.title,
            namespace: r.namespace,
            repo_name: r.repo_name,
            author: r.author,
            updated_at: r.updated_at,
        })
        .collect())
}

/// Whether revision `seq` kept the changes and patches of the previous
/// per-change revision.
pub async fn set_same_patches(
    exec: impl PgExecutor<'_>,
    mr: MergeRequestId,
    seq: i32,
    same: bool,
) -> Result<(), DbError> {
    sqlx::query!(
        "UPDATE mr_revisions SET same_patches = $3 WHERE mr_id = $1 AND seq = $2",
        mr.as_uuid(),
        seq,
        same
    )
    .execute(exec)
    .await?;
    Ok(())
}

/// `same_patches` of every revision, by revision number.
pub async fn same_patches(
    exec: impl PgExecutor<'_>,
    mr: MergeRequestId,
) -> Result<Vec<(i32, Option<bool>)>, DbError> {
    let rows = sqlx::query!(
        "SELECT seq, same_patches FROM mr_revisions WHERE mr_id = $1 ORDER BY seq",
        mr.as_uuid()
    )
    .fetch_all(exec)
    .await?;
    Ok(rows.into_iter().map(|r| (r.seq, r.same_patches)).collect())
}
