// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Commit statuses: reports by authenticated reporters on a commit.

use slath_core::ids::{RepoId, UserId};
use sqlx::PgExecutor;

use crate::DbError;

/// One reported status.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Status {
    pub id: uuid::Uuid,
    pub commit_oid: String,
    pub context: String,
    /// `pending`, `success`, `failure`, or `error`.
    pub state: String,
    pub description: Option<String>,
    pub target_url: Option<String>,
    /// The reporting user or bot.
    pub reporter_id: Option<UserId>,
    pub reporter: Option<String>,
    pub created_at: time::OffsetDateTime,
    /// Its line annotations, in the order reported.
    pub annotations: Vec<Annotation>,
}

/// A line annotation (finding) of a status.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Annotation {
    pub path: String,
    pub line_start: i32,
    pub line_end: i32,
    /// `notice`, `warning`, or `failure`.
    pub severity: String,
    pub category: String,
    pub message: String,
}

/// What a new status records.
#[derive(Debug)]
pub struct NewStatus<'a> {
    pub repo: RepoId,
    pub commit_oid: &'a str,
    pub context: &'a str,
    pub state: &'a str,
    pub description: Option<&'a str>,
    pub target_url: Option<&'a str>,
    pub reporter: UserId,
    pub annotations: &'a [Annotation],
}

/// Records a status and its annotations.
pub async fn insert(
    conn: &mut sqlx::PgConnection,
    s: &NewStatus<'_>,
) -> Result<uuid::Uuid, DbError> {
    let id = uuid::Uuid::now_v7();
    sqlx::query!(
        "INSERT INTO commit_statuses (id, repo_id, commit_oid, context, state, description, target_url,
             creator_kind, creator_id)
         VALUES ($1, $2, $3, $4, $5, $6, $7, 'user', $8)",
        id,
        s.repo.as_uuid(),
        s.commit_oid,
        s.context,
        s.state,
        s.description,
        s.target_url,
        s.reporter.as_uuid()
    )
    .execute(&mut *conn)
    .await?;
    if !s.annotations.is_empty() {
        let a = s.annotations;
        let ordinal: Vec<i32> = (0..).take(a.len()).collect();
        let path: Vec<String> = a.iter().map(|x| x.path.clone()).collect();
        let start: Vec<i32> = a.iter().map(|x| x.line_start).collect();
        let end: Vec<i32> = a.iter().map(|x| x.line_end).collect();
        let severity: Vec<String> = a.iter().map(|x| x.severity.clone()).collect();
        let category: Vec<String> = a.iter().map(|x| x.category.clone()).collect();
        let message: Vec<String> = a.iter().map(|x| x.message.clone()).collect();
        sqlx::query!(
            "INSERT INTO status_annotations (status_id, ordinal, path, line_start, line_end, severity,
                 category, message)
             SELECT $1, * FROM UNNEST($2::int[], $3::text[], $4::int[], $5::int[], $6::text[],
                 $7::text[], $8::text[])",
            id,
            &ordinal,
            &path,
            &start,
            &end,
            &severity,
            &category,
            &message
        )
        .execute(&mut *conn)
        .await?;
    }
    Ok(id)
}

/// How many statuses a commit has for a context.
pub async fn count(
    exec: impl PgExecutor<'_>,
    repo: RepoId,
    oid: &str,
    context: &str,
) -> Result<i64, DbError> {
    Ok(sqlx::query_scalar!(
        r#"SELECT count(*) AS "n!" FROM commit_statuses WHERE repo_id = $1 AND commit_oid = $2 AND context = $3"#,
        repo.as_uuid(),
        oid,
        context
    )
    .fetch_one(exec)
    .await?)
}

/// Every status of a commit, newest first.
pub async fn for_commit(
    exec: impl PgExecutor<'_> + Copy,
    repo: RepoId,
    oid: &str,
) -> Result<Vec<Status>, DbError> {
    let rows = sqlx::query!(
        r#"SELECT s.id, s.commit_oid, s.context, s.state, s.description, s.target_url, s.creator_id,
                  n.name::text AS reporter, s.created_at
           FROM commit_statuses s
           LEFT JOIN users u ON u.id = s.creator_id AND u.state <> 'deleted'
           LEFT JOIN namespaces n ON n.id = u.namespace_id
           WHERE s.repo_id = $1 AND s.commit_oid = $2
           ORDER BY s.created_at DESC, s.id DESC"#,
        repo.as_uuid(),
        oid
    )
    .fetch_all(exec)
    .await?;
    let mut notes = sqlx::query!(
        r#"SELECT a.status_id, a.path, a.line_start, a.line_end, a.severity, a.category, a.message
           FROM status_annotations a JOIN commit_statuses s ON s.id = a.status_id
           WHERE s.repo_id = $1 AND s.commit_oid = $2
           ORDER BY a.status_id, a.ordinal"#,
        repo.as_uuid(),
        oid
    )
    .fetch_all(exec)
    .await?;
    let mut annotations = std::collections::HashMap::<uuid::Uuid, Vec<Annotation>>::new();
    for n in notes.drain(..) {
        annotations
            .entry(n.status_id)
            .or_default()
            .push(Annotation {
                path: n.path,
                line_start: n.line_start,
                line_end: n.line_end,
                severity: n.severity,
                category: n.category,
                message: n.message,
            });
    }
    Ok(rows
        .into_iter()
        .map(|r| Status {
            annotations: annotations.remove(&r.id).unwrap_or_default(),
            id: r.id,
            commit_oid: r.commit_oid,
            context: r.context,
            state: r.state,
            description: r.description,
            target_url: r.target_url,
            reporter_id: r.creator_id.map(UserId::from_uuid),
            reporter: r.reporter,
            created_at: r.created_at,
        })
        .collect())
}
