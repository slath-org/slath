// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Readiness for lists, from a derived snapshot: each open merge request's
//! readiness card tone and headline, and its row's check and approval
//! states, as last computed. Database triggers
//! mark a snapshot outdated on every change that can affect readiness; an
//! outdated one is computed again when a list shows it, and stored only if
//! nothing changed meanwhile. Nothing but lists reads it: merging and every
//! merge request page compute readiness live.

use std::collections::HashMap;

use slath_core::authz::{Action, Subject};
use slath_core::ids::{MergeRequestId, RepoId};
use slath_db::merge_requests::MergeRequest;
use slath_db::mr_readiness as db;
pub use slath_db::mr_readiness::Readiness;
use slath_db::statuses::Status;

use crate::Services;
use crate::error::{Result, ServiceError};
use crate::mr_reviews::{Mergeability, readiness_summary};
use crate::repos::RepoAccess;

/// The latest checks of a head in one tone (`done`, `waiting`,
/// `blocking`) and words: failing if any failed, running if any is
/// pending, passed if all passed; `None` without any.
#[must_use]
pub fn checks_state(statuses: &[Status]) -> Option<(&'static str, String)> {
    let total = statuses.len();
    if total == 0 {
        return None;
    }
    let count = |states: &[&str]| {
        statuses
            .iter()
            .filter(|s| states.contains(&s.state.as_str()))
            .count()
    };
    let failed = count(&["failure", "error"]);
    let pending = count(&["pending"]);
    let checks = if total == 1 { "check" } else { "checks" };
    Some(if failed > 0 {
        ("blocking", format!("{failed} of {total} {checks} failed"))
    } else if pending > 0 {
        ("waiting", format!("{pending} of {total} {checks} running"))
    } else if total == 1 {
        ("done", "The check passed".to_owned())
    } else {
        ("done", format!("All {total} checks passed"))
    })
}

/// The approvals of a merge request in one tone and words, from its
/// requirements: a request for changes first, then missing approvals,
/// then a counting approval; `None` when none are required and none were
/// given, so rows without review rules stay quiet.
#[must_use]
pub fn approvals_state(mb: &Mergeability) -> Option<(&'static str, &'static str)> {
    let unmet = |key: &str| {
        mb.requirements
            .iter()
            .any(|r| r.key == key && r.state != "met")
    };
    if unmet("changes_requested") {
        return Some(("blocking", "Changes requested"));
    }
    if unmet("approvals") {
        return Some(("waiting", "Approval required"));
    }
    if unmet("sensitive_paths") {
        return Some(("waiting", "Maintainer approval required"));
    }
    mb.requirements
        .iter()
        .any(|r| r.key == "approvals" && r.reason != "No approvals are required.")
        .then_some(("done", "Approved"))
}

/// A merge request's readiness as a list shows it, from the live
/// computation the readiness card uses and its head's latest statuses.
#[must_use]
pub fn list_readiness(m: &MergeRequest, mb: &Mergeability, statuses: &[Status]) -> Readiness {
    let (tone, headline) = readiness_summary(m, mb);
    let owned = |(t, w): (&str, String)| (t.to_owned(), w);
    Readiness {
        tone: tone.to_owned(),
        headline,
        checks: checks_state(statuses).map(owned),
        approvals: approvals_state(mb).map(|(t, w)| (t.to_owned(), w.to_owned())),
    }
}

impl Services {
    /// The readiness of these merge requests for a list, in the same order;
    /// `None` for those that aren't open. Outdated snapshots are computed
    /// again, concurrently, from the merge request as it is now.
    pub async fn mr_list_readiness(
        &self,
        a: &RepoAccess,
        ms: &[MergeRequest],
    ) -> Result<Vec<Option<Readiness>>> {
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
                if let Some(r) = snap.and_then(db::Snapshot::current) {
                    return Ok(Some(r.clone()));
                }
                // One merge request that can't be computed (its commits
                // gone, say) shows no icon rather than failing the list.
                match self
                    .rebuild_readiness(a, m.id, snap.map(|s| s.generation))
                    .await
                {
                    Ok(r) => Ok(r),
                    Err(e) => {
                        tracing::warn!(error = %e, mr = %m.id, "computing a merge request's readiness failed");
                        Ok(None)
                    }
                }
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
    ) -> Result<Option<Readiness>> {
        let Some(m) = slath_db::merge_requests::get_by_id(&self.pool, id).await? else {
            return Ok(None);
        };
        if m.state != "open" {
            return Ok(None);
        }
        let r = self.live_readiness(a, &m).await?;
        if let Some(g) = generation {
            db::store(&self.pool, id, g, &r).await?;
        }
        Ok(Some(r))
    }

    /// One merge request's readiness for a list, computed live.
    async fn live_readiness(&self, a: &RepoAccess, m: &MergeRequest) -> Result<Readiness> {
        let mb = self.mr_mergeability(a, m).await?;
        let statuses = self.mr_statuses(m).await?;
        Ok(list_readiness(m, &mb, &statuses))
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
            // What can't be computed is left for the checks that report
            // it (a revision's missing commits).
            let live = match self.live_readiness(&a, &m).await {
                Ok(r) => r,
                Err(e) => {
                    tracing::warn!(error = %e, mr = %m.id, "computing a merge request's readiness failed");
                    continue;
                }
            };
            let stored = s.current();
            if stored.is_some_and(|r| *r != live) {
                differences += 1;
            }
            if repair && stored != Some(&live) {
                db::store(&self.pool, s.mr_id, s.generation, &live).await?;
            }
        }
        Ok(differences)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mr_reviews::{met, unmet};

    fn mb(requirements: Vec<crate::mr_reviews::Requirement>) -> Mergeability {
        Mergeability {
            requirements,
            ..Mergeability::default()
        }
    }

    #[test]
    fn approvals_state_puts_requested_changes_first_and_stays_quiet_without_rules() {
        assert_eq!(approvals_state(&mb(Vec::new())), None);
        assert_eq!(
            approvals_state(&mb(vec![met("approvals", "No approvals are required.")])),
            None
        );
        assert_eq!(
            approvals_state(&mb(vec![met("approvals", "Approved by bob.")])),
            Some(("done", "Approved"))
        );
        assert_eq!(
            approvals_state(&mb(vec![
                met("approvals", "Approved by bob."),
                unmet("sensitive_paths", "x", "y"),
            ])),
            Some(("waiting", "Maintainer approval required"))
        );
        assert_eq!(
            approvals_state(&mb(vec![
                unmet("approvals", "0 of 1 required approvals.", "y"),
                unmet("changes_requested", "x", "y"),
            ])),
            Some(("blocking", "Changes requested"))
        );
    }
}
