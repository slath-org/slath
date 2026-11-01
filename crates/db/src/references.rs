// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Candidates for reference autocomplete. Callers check that the subject
//! can read the repository; these queries never return email addresses.

use slath_core::ids::{RepoId, UserId};
use sqlx::PgExecutor;

use crate::DbError;

/// A `LIKE` pattern matching values that start with `q` (escaped).
#[must_use]
pub fn prefix_pattern(q: &str) -> String {
    let mut p = escape_like(q);
    p.push('%');
    p
}

/// A `LIKE` pattern matching values that contain `q` (escaped).
#[must_use]
pub fn contains_pattern(q: &str) -> String {
    format!("%{}%", escape_like(q))
}

fn escape_like(q: &str) -> String {
    let mut out = String::with_capacity(q.len());
    for c in q.chars() {
        if matches!(c, '%' | '_' | '\\') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserCandidate {
    pub name: String,
    pub display_name: Option<String>,
    /// 0: takes part in the thread, 1: has access to the repository, 2: other.
    pub rank: i32,
}

/// Active users whose name or display name starts with the query, ranked:
/// participants of `item` first, then people with a grant on the repository,
/// then everyone else. With `participants_only`, only people who already
/// took part in the repository (authors of its issues and comments,
/// assignees, and its owner) are returned.
pub async fn users(
    exec: impl PgExecutor<'_>,
    repo: RepoId,
    item: Option<uuid::Uuid>,
    q: &str,
    participants_only: bool,
    limit: i64,
) -> Result<Vec<UserCandidate>, DbError> {
    let rows = sqlx::query!(
        r#"WITH thread AS (
               SELECT author_id AS id FROM issues WHERE id = $2
               UNION SELECT author_id FROM comments WHERE subject_id = $2 AND deleted_at IS NULL
               UNION SELECT user_id FROM issue_assignees WHERE issue_id = $2
           ), repo_people AS (
               SELECT author_id AS id FROM issues WHERE repo_id = $1
               UNION SELECT author_id FROM comments WHERE repo_id = $1 AND deleted_at IS NULL
               UNION SELECT a.user_id FROM issue_assignees a JOIN issues i ON i.id = a.issue_id
                      WHERE i.repo_id = $1
               UNION SELECT u.id FROM users u JOIN repositories r ON r.namespace_id = u.namespace_id
                      WHERE r.id = $1
           )
           SELECT n.name::text AS "name!", u.display_name,
                  (CASE WHEN u.id IN (SELECT id FROM thread WHERE id IS NOT NULL) THEN 0
                        WHEN slath_can_read($1, u.id) THEN 1
                        ELSE 2 END) AS "rank!"
             FROM users u JOIN namespaces n ON n.id = u.namespace_id
            WHERE u.state = 'active'
              AND (n.name::text ILIKE $3 OR u.display_name ILIKE $3)
              AND (NOT $4 OR u.id IN (SELECT id FROM repo_people WHERE id IS NOT NULL))
            ORDER BY 3, lower(n.name::text)
            LIMIT $5"#,
        repo.as_uuid(),
        item,
        prefix_pattern(q),
        participants_only,
        limit
    )
    .fetch_all(exec)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| UserCandidate {
            name: r.name,
            display_name: r.display_name,
            rank: r.rank,
        })
        .collect())
}

/// Whether the user has a grant on the repository (not just public read).
pub async fn has_grant(
    exec: impl PgExecutor<'_>,
    repo: RepoId,
    user: UserId,
) -> Result<bool, DbError> {
    Ok(sqlx::query_scalar!(
        r#"SELECT slath_can_read($1, $2) AS "ok!""#,
        repo.as_uuid(),
        user.as_uuid()
    )
    .fetch_one(exec)
    .await?)
}

/// Issues by number prefix first, then by title substring.
pub async fn items(
    exec: impl PgExecutor<'_>,
    repo: RepoId,
    q: &str,
    limit: i64,
) -> Result<Vec<(i32, String, String)>, DbError> {
    let digits = !q.is_empty() && q.bytes().all(|b| b.is_ascii_digit());
    let rows = sqlx::query!(
        r#"SELECT number, title, state FROM issues
            WHERE repo_id = $1
              AND (($2 AND number::text LIKE $3) OR title ILIKE $4 OR $5 = '')
            ORDER BY (CASE WHEN $2 AND number::text LIKE $3 THEN 0 ELSE 1 END),
                     updated_at DESC
            LIMIT $6"#,
        repo.as_uuid(),
        digits,
        prefix_pattern(q),
        contains_pattern(q),
        q,
        limit
    )
    .fetch_all(exec)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| (r.number, r.title, r.state))
        .collect())
}

/// Labels as `(name, color, description)`: prefix matches first.
pub async fn labels(
    exec: impl PgExecutor<'_>,
    repo: RepoId,
    q: &str,
    limit: i64,
) -> Result<Vec<(String, String, Option<String>)>, DbError> {
    let rows = sqlx::query!(
        r#"SELECT name::text AS "name!", color::text AS "color!", description FROM labels
            WHERE repo_id = $1 AND name::text ILIKE $3
            ORDER BY (CASE WHEN name::text ILIKE $2 THEN 0 ELSE 1 END), lower(name::text)
            LIMIT $4"#,
        repo.as_uuid(),
        prefix_pattern(q),
        contains_pattern(q),
        limit
    )
    .fetch_all(exec)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| (r.name, r.color, r.description))
        .collect())
}

/// Milestones as `(title, state)`: open and prefix matches first.
pub async fn milestones(
    exec: impl PgExecutor<'_>,
    repo: RepoId,
    q: &str,
    limit: i64,
) -> Result<Vec<(String, String)>, DbError> {
    let rows = sqlx::query!(
        r#"SELECT title::text AS "title!", state FROM milestones
            WHERE repo_id = $1 AND title::text ILIKE $3
            ORDER BY (CASE WHEN title::text ILIKE $2 THEN 0 ELSE 1 END),
                     (state = 'closed'), lower(title::text)
            LIMIT $4"#,
        repo.as_uuid(),
        prefix_pattern(q),
        contains_pattern(q),
        limit
    )
    .fetch_all(exec)
    .await?;
    Ok(rows.into_iter().map(|r| (r.title, r.state)).collect())
}

/// Teams of the organization owning the repository, as `org/team`, for a
/// subject who is a member of that organization.
pub async fn teams(
    exec: impl PgExecutor<'_>,
    repo: RepoId,
    member: UserId,
    q: &str,
    limit: i64,
) -> Result<Vec<String>, DbError> {
    Ok(sqlx::query_scalar!(
        r#"SELECT n.name::text || '/' || t.name::text AS "name!"
             FROM repositories r
             JOIN organizations o ON o.namespace_id = r.namespace_id
             JOIN namespaces n ON n.id = o.namespace_id
             JOIN teams t ON t.org_id = o.id
            WHERE r.id = $1
              AND EXISTS (SELECT 1 FROM org_members m WHERE m.org_id = o.id AND m.user_id = $2)
              AND t.name::text ILIKE $3
            ORDER BY lower(t.name::text)
            LIMIT $4"#,
        repo.as_uuid(),
        member.as_uuid(),
        prefix_pattern(q),
        limit
    )
    .fetch_all(exec)
    .await?)
}

/// A label of a repository by name (any case), as `(name, description)`.
pub async fn label(
    exec: impl PgExecutor<'_>,
    repo: RepoId,
    name: &str,
) -> Result<Option<(String, Option<String>)>, DbError> {
    let r = sqlx::query!(
        r#"SELECT name::text AS "name!", description FROM labels
            WHERE repo_id = $1 AND name = $2::text::citext"#,
        repo.as_uuid(),
        name
    )
    .fetch_optional(exec)
    .await?;
    Ok(r.map(|r| (r.name, r.description)))
}

/// A milestone of a repository by title (any case), as `(title, state)`.
pub async fn milestone(
    exec: impl PgExecutor<'_>,
    repo: RepoId,
    title: &str,
) -> Result<Option<(String, String)>, DbError> {
    let r = sqlx::query!(
        r#"SELECT title::text AS "title!", state FROM milestones
            WHERE repo_id = $1 AND title = $2::text::citext"#,
        repo.as_uuid(),
        title
    )
    .fetch_optional(exec)
    .await?;
    Ok(r.map(|r| (r.title, r.state)))
}
