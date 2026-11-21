// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Readiness for lists, from a derived snapshot: each open merge request's
//! readiness card tone and headline as last computed. Database triggers
//! mark a snapshot outdated on every change that can affect readiness; an
//! outdated one is computed again when a list shows it, and stored only if
//! nothing changed meanwhile. Nothing but lists reads it: merging and every
//! merge request page compute readiness live.

use std::collections::HashMap;

use slath_core::authz::{Action, Subject};
use slath_core::ids::{MergeRequestId, RepoId};
use slath_db::merge_requests::MergeRequest;
use slath_db::mr_readiness as db;

use crate::Services;
use crate::error::{Result, ServiceError};
use crate::mr_reviews::readiness_summary;
use crate::repos::RepoAccess;

/// A merge request's readiness in short: the tone and the headline.
pub type Summary = (String, String);

impl Services {
    /// The readiness of these merge requests for a list, in the same order;
    /// `None` for those that aren't open. Outdated snapshots are computed
    /// again, concurrently, from the merge request as it is now.
    pub async fn mr_list_readiness(
        &self,
        a: &RepoAccess,
        ms: &[MergeRequest],
    ) -> Result<Vec<Option<Summary>>> {
        let open: Vec<MergeRequestId> = ms
            .iter()
            .filter(|m| m.state == "open")
            .map(|m| m.id)
            .collect();
        let snaps: HashMap<MergeRequestId, db::Snapshot> = db::get_many(&self.pool, &open)
            .await?
            .into_iter()
            .map(|s| (s.mr_id, s))
            .collect();
        futures_util::future::try_join_all(ms.iter().map(|m| {
            let snap = snaps.get(&m.id);
            async move {
                if m.state != "open" {
                    return Ok(None);
                }
                if let Some((tone, headline)) = snap.and_then(db::Snapshot::current) {
                    return Ok(Some((tone.to_owned(), headline.to_owned())));
                }
                self.rebuild_readiness(a, m.id, snap.map(|s| s.generation))
                    .await
            }
        }))
        .await
    }

    /// Computes one merge request's readiness live and stores it as of
    /// `generation` (read before), unless something changed since. The
    /// merge request is read again, as the caller's copy may be older than
    /// the generation.
    async fn rebuild_readiness(
        &self,
        a: &RepoAccess,
        id: MergeRequestId,
        generation: Option<i64>,
    ) -> Result<Option<Summary>> {
        let Some(m) = slath_db::merge_requests::get_by_id(&self.pool, id).await? else {
            return Ok(None);
        };
        if m.state != "open" {
            return Ok(None);
        }
        let mb = self.mr_mergeability(a, &m).await?;
        let (tone, headline) = readiness_summary(&m, &mb);
        if let Some(g) = generation {
            db::store(&self.pool, id, g, tone, &headline).await?;
        }
        Ok(Some((tone.to_owned(), headline)))
    }

    /// Compares the current snapshots of a repository's open merge requests
    /// with a live computation; the number that differ. With `repair`, the
    /// live result is stored.
    pub(crate) async fn check_readiness(&self, repo: RepoId, repair: bool) -> Result<usize> {
        let a = match self
            .repo_access_by_id(&Subject::System, repo, Action::RepoRead)
            .await
        {
            Ok(a) => a,
            Err(ServiceError::NotFound | ServiceError::Forbidden) => return Ok(0),
            Err(e) => return Err(e),
        };
        let ids = slath_db::merge_requests::open_ids(&self.pool, repo).await?;
        let mut differences = 0;
        for s in db::get_many(&self.pool, &ids).await? {
            let Some(m) = slath_db::merge_requests::get_by_id(&self.pool, s.mr_id).await? else {
                continue;
            };
            let mb = self.mr_mergeability(&a, &m).await?;
            let (tone, headline) = readiness_summary(&m, &mb);
            let stored = s.current();
            if stored.is_some_and(|(t, h)| t != tone || h != headline) {
                differences += 1;
            }
            if repair && stored != Some((tone, headline.as_str())) {
                db::store(&self.pool, s.mr_id, s.generation, tone, &headline).await?;
            }
        }
        Ok(differences)
    }
}
