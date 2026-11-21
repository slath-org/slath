// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! The readiness snapshot of merge requests, for lists: each one's
//! readiness card tone and headline as last computed. Derived and never
//! trusted for merging. Database triggers bump `generation` on every
//! change that can affect readiness; a snapshot is current while
//! `built_generation = generation`.

use slath_core::ids::MergeRequestId;
use sqlx::PgPool;

use crate::DbError;

/// One merge request's snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
    pub mr_id: MergeRequestId,
    /// Bumped by every change that can affect readiness.
    pub generation: i64,
    /// The `generation` the stored tone and headline were computed at.
    pub built_generation: Option<i64>,
    pub tone: Option<String>,
    pub headline: Option<String>,
}

impl Snapshot {
    /// The stored tone and headline, if nothing changed since.
    #[must_use]
    pub fn current(&self) -> Option<(&str, &str)> {
        match (self.built_generation, &self.tone, &self.headline) {
            (Some(b), Some(t), Some(h)) if b == self.generation => Some((t, h)),
            _ => None,
        }
    }
}

/// The snapshots of these merge requests, creating empty ones (never
/// built) for those that have none yet, in no particular order.
pub async fn get_many(pool: &PgPool, ids: &[MergeRequestId]) -> Result<Vec<Snapshot>, DbError> {
    let ids: Vec<uuid::Uuid> = ids.iter().map(|i| *i.as_uuid()).collect();
    sqlx::query!(
        "INSERT INTO mr_readiness (mr_id)
         SELECT m.id FROM merge_requests m WHERE m.id = ANY($1)
         ON CONFLICT (mr_id) DO NOTHING",
        &ids
    )
    .execute(pool)
    .await?;
    let rows = sqlx::query!(
        "SELECT mr_id, generation, built_generation, tone, headline FROM mr_readiness WHERE mr_id = ANY($1)",
        &ids
    )
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| Snapshot {
            mr_id: MergeRequestId::from_uuid(r.mr_id),
            generation: r.generation,
            built_generation: r.built_generation,
            tone: r.tone,
            headline: r.headline,
        })
        .collect())
}

/// Stores a snapshot computed after reading `generation`; does nothing (and
/// returns false) if something changed meanwhile.
pub async fn store(
    pool: &PgPool,
    id: MergeRequestId,
    generation: i64,
    tone: &str,
    headline: &str,
) -> Result<bool, DbError> {
    let r = sqlx::query!(
        "UPDATE mr_readiness SET built_generation = generation, tone = $3, headline = $4, built_at = now()
         WHERE mr_id = $1 AND generation = $2",
        id.as_uuid(),
        generation,
        tone,
        headline
    )
    .execute(pool)
    .await?;
    Ok(r.rows_affected() == 1)
}
