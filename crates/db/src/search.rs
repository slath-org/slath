// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Search. Visibility is part of every query (before ranking and LIMIT), so
//! pages are full and nothing invisible is ever counted or returned.

use slath_core::ids::{RepoId, UserId};
use sqlx::PgExecutor;
use time::OffsetDateTime;

use crate::DbError;

/// Who is searching, as the visibility filter needs it.
#[derive(Debug, Clone, Default)]
pub struct Viewer {
    /// `None` for anonymous visitors (and tokens without `repo:read`).
    pub user: Option<UserId>,
    pub site_admin: bool,
    /// A token restricted to these repositories (plus public ones).
    pub only: Option<Vec<RepoId>>,
}

impl Viewer {
    fn parts(&self) -> (Option<uuid::Uuid>, bool, Option<Vec<uuid::Uuid>>) {
        (
            self.user.map(|u| *u.as_uuid()),
            self.site_admin,
            self.only
                .as_ref()
                .map(|v| v.iter().map(|r| *r.as_uuid()).collect()),
        )
    }
}

#[derive(Debug, Clone)]
pub struct IssueHit {
    pub repo: String,
    pub number: i32,
    pub title: String,
    pub state: String,
    pub updated_at: OffsetDateTime,
}

/// Issues whose title, description, or a visible comment match `q`
/// (`websearch_to_tsquery` syntax), best first. `repo` limits the search to
/// one repository.
pub async fn issues(
    exec: impl PgExecutor<'_>,
    viewer: &Viewer,
    q: &str,
    repo: Option<RepoId>,
    offset: i64,
    limit: i64,
) -> Result<Vec<IssueHit>, DbError> {
    let (user, admin, only) = viewer.parts();
    let rows = sqlx::query!(
        r#"WITH q AS (SELECT websearch_to_tsquery('simple', $1) AS query),
           visible AS (
               SELECT r.id FROM repositories r
                WHERE r.state <> 'deleting' AND r.issues_enabled
                  AND ($6::uuid IS NULL OR r.id = $6)
                  AND (r.visibility = 'public' OR $3
                       OR ((slath_can_read(r.id, $2::uuid)
                            OR (r.visibility = 'internal' AND $2::uuid IS NOT NULL))
                           AND ($4::uuid[] IS NULL OR r.id = ANY($4))))
           ),
           hits AS (
               SELECT i.id, ts_rank(i.search, q.query) AS rank FROM issues i, q
                WHERE i.repo_id IN (SELECT id FROM visible) AND i.search @@ q.query
               UNION ALL
               SELECT c.subject_id, ts_rank(c.search, q.query) * 0.5 FROM comments c, q
                WHERE c.repo_id IN (SELECT id FROM visible) AND c.subject_kind = 'issue'
                  AND c.deleted_at IS NULL AND c.hidden_at IS NULL AND c.search @@ q.query
           ),
           best AS (SELECT id, max(rank) AS rank FROM hits GROUP BY id)
           SELECT n.name::text || '/' || r.name::text AS "repo!", i.number, i.title, i.state, i.updated_at
             FROM best b JOIN issues i ON i.id = b.id
             JOIN repositories r ON r.id = i.repo_id JOIN namespaces n ON n.id = r.namespace_id
            ORDER BY b.rank DESC, i.updated_at DESC, i.id
           OFFSET $7 LIMIT $5"#,
        q,
        user,
        admin,
        only.as_deref(),
        limit,
        repo.map(|r| *r.as_uuid()),
        offset,
    )
    .fetch_all(exec)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| IssueHit {
            repo: r.repo,
            number: r.number,
            title: r.title,
            state: r.state,
            updated_at: r.updated_at,
        })
        .collect())
}

#[derive(Debug, Clone)]
pub struct RepoHit {
    pub namespace: String,
    pub name: String,
    pub description: Option<String>,
    pub private: bool,
    /// `public`, `internal`, or `private`.
    pub visibility: String,
}

/// Repositories whose name or description contains `q`, most similar first.
pub async fn repos(
    exec: impl PgExecutor<'_>,
    viewer: &Viewer,
    q: &str,
    offset: i64,
    limit: i64,
) -> Result<Vec<RepoHit>, DbError> {
    let (user, admin, only) = viewer.parts();
    let pattern = crate::references::contains_pattern(q);
    let rows = sqlx::query!(
        r#"SELECT n.name::text AS "namespace!", r.name::text AS "name!", r.description, r.visibility
             FROM repositories r JOIN namespaces n ON n.id = r.namespace_id
            WHERE r.state <> 'deleting'
              AND (r.visibility = 'public' OR $3
                   OR ((slath_can_read(r.id, $2::uuid)
                        OR (r.visibility = 'internal' AND $2::uuid IS NOT NULL))
                       AND ($4::uuid[] IS NULL OR r.id = ANY($4))))
              AND (r.name::text ILIKE $1 OR r.description ILIKE $1
                   OR (n.name::text || '/' || r.name::text) ILIKE $1)
            ORDER BY greatest(similarity(r.name::text, $5), similarity(coalesce(r.description, ''), $5)) DESC,
                     lower(n.name::text), lower(r.name::text)
           OFFSET $6 LIMIT $7"#,
        pattern,
        user,
        admin,
        only.as_deref(),
        q,
        offset,
        limit,
    )
    .fetch_all(exec)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| RepoHit {
            namespace: r.namespace,
            name: r.name,
            description: r.description,
            private: r.visibility != "public",
            visibility: r.visibility,
        })
        .collect())
}
