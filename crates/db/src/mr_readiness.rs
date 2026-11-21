// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! The readiness snapshot of merge requests, for lists: each one's
//! readiness card tone and headline, and its row's check and approval
//! states, as last computed. Derived and never trusted for merging.
//! Database triggers bump `generation` on every change that can affect
//! readiness; a snapshot is current while `built_generation = generation`
//! and it has this code's format.

use slath_core::ids::MergeRequestId;
use sqlx::PgPool;

use crate::DbError;

/// The snapshot format this code writes: tone, headline, and the row's
/// check and approval states. A snapshot of another format isn't current.
pub const FORMAT: i16 = 2;

/// A merge request's readiness as a list shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Readiness {
    /// The readiness card's tone: `done`, `waiting`, or `blocking`.
    pub tone: String,
    /// The readiness card's headline.
    pub headline: String,
    /// The head's checks: a tone and words; `None` when none reported.
    pub checks: Option<(String, String)>,
    /// The approvals: a tone and words; `None` when none are required
    /// and none were given.
    pub approvals: Option<(String, String)>,
}

/// One merge request's snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
    pub mr_id: MergeRequestId,
    /// Bumped by every change that can affect readiness.
    pub generation: i64,
    /// The `generation` the stored readiness was computed at.
    pub built_generation: Option<i64>,
    pub built_format: i16,
    pub readiness: Option<Readiness>,
}

impl Snapshot {
    /// The stored readiness, if nothing changed since and it has this
    /// code's format.
    #[must_use]
    pub fn current(&self) -> Option<&Readiness> {
        match (self.built_generation, &self.readiness) {
            (Some(b), Some(r)) if b == self.generation && self.built_format == FORMAT => Some(r),
            _ => None,
        }
    }
}

fn pair(tone: Option<String>, text: Option<String>) -> Option<(String, String)> {
    tone.zip(text)
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
        "SELECT mr_id, generation, built_generation, built_format, tone, headline,
                checks_tone, checks_text, approvals_tone, approvals_text
           FROM mr_readiness WHERE mr_id = ANY($1)",
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
            built_format: r.built_format,
            readiness: r.tone.zip(r.headline).map(|(tone, headline)| Readiness {
                tone,
                headline,
                checks: pair(r.checks_tone, r.checks_text),
                approvals: pair(r.approvals_tone, r.approvals_text),
            }),
        })
        .collect())
}

/// Stores a snapshot computed after reading `generation`; does nothing (and
/// returns false) if something changed meanwhile.
pub async fn store(
    pool: &PgPool,
    id: MergeRequestId,
    generation: i64,
    r: &Readiness,
) -> Result<bool, DbError> {
    let (checks_tone, checks_text) = r.checks.clone().unzip();
    let (approvals_tone, approvals_text) = r.approvals.clone().unzip();
    let r = sqlx::query!(
        "UPDATE mr_readiness SET built_generation = generation, built_format = $3, tone = $4, headline = $5,
                checks_tone = $6, checks_text = $7, approvals_tone = $8, approvals_text = $9, built_at = now()
         WHERE mr_id = $1 AND generation = $2",
        id.as_uuid(),
        generation,
        FORMAT,
        r.tone,
        r.headline,
        checks_tone,
        checks_text,
        approvals_tone,
        approvals_text
    )
    .execute(pool)
    .await?;
    Ok(r.rows_affected() == 1)
}
