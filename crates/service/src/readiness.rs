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

/// A page of a repository's merge request list with each one's readiness.
pub struct ListPage {
    pub access: RepoAccess,
    /// The merge requests of the page, with the readiness of open ones.
    pub items: Vec<(MergeRequest, Option<Readiness>)>,
    /// `(open, merged, closed)` counts matching the filter.
    pub counts: (i64, i64, i64),
    /// For open lists, which are grouped by readiness: how many matching
    /// merge requests each `(tone, headline)` has, `None` for those not
    /// computed yet.
    pub groups: Option<Vec<slath_db::merge_requests::GroupCount>>,
}

/// A built-in view of a merge request list: a filter that combines with
/// the others, never a grant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum View {
    All,
    /// The viewer, or a team of theirs, is asked to review it.
    ReviewRequests,
    /// The viewer wrote it.
    Mine,
    /// Open without activity for [`STALLED_AFTER`].
    Stalled,
    /// Ready to merge, from the snapshot.
    Ready,
}

/// How long an open merge request goes without activity (anything that
/// updates it: a push, comment, review, or edit) before it's stalled.
pub const STALLED_AFTER: time::Duration = time::Duration::days(14);

impl View {
    pub const ALL: [View; 5] = [
        View::All,
        View::ReviewRequests,
        View::Mine,
        View::Stalled,
        View::Ready,
    ];

    /// The view a `view` query parameter names; `All` for anything else.
    #[must_use]
    pub fn parse(s: &str) -> View {
        Self::ALL
            .into_iter()
            .find(|v| v.key() == s)
            .unwrap_or(View::All)
    }

    /// Its `view` query parameter; empty for `All`.
    #[must_use]
    pub fn key(self) -> &'static str {
        match self {
            View::All => "",
            View::ReviewRequests => "review-requests",
            View::Mine => "mine",
            View::Stalled => "stalled",
            View::Ready => "ready",
        }
    }

    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            View::All => "All",
            View::ReviewRequests => "Review requests",
            View::Mine => "Mine",
            View::Stalled => "Stalled",
            View::Ready => "Ready to merge",
        }
    }

    /// Whether it means something only to a signed-in viewer.
    #[must_use]
    pub fn personal(self) -> bool {
        matches!(self, View::ReviewRequests | View::Mine)
    }

    /// The list options it adds for `subject`; personal views need a
    /// signed-in user.
    fn options(self, subject: &Subject) -> Result<slath_db::merge_requests::ListOptions> {
        let mut o = slath_db::merge_requests::ListOptions::default();
        let user = || match subject {
            Subject::User { id, .. } => Ok(*id),
            _ => Err(ServiceError::Unauthenticated),
        };
        match self {
            View::All => {}
            View::ReviewRequests => o.reviewer = Some(user()?),
            View::Mine => o.mine = Some(user()?),
            View::Stalled => {
                o.stalled_before = Some(time::OffsetDateTime::now_utc() - STALLED_AFTER);
            }
            View::Ready => o.ready = Some(crate::mr_reviews::READY),
        }
        Ok(o)
    }
}

/// Where a readiness tone's group comes in an open list: ready to merge
/// first, then where someone must act, then waiting.
#[must_use]
pub fn group_rank(tone: &str) -> u8 {
    match tone {
        "done" => 0,
        "blocking" => 1,
        _ => 2,
    }
}

/// Puts the rows of a page of an open list in group order, keeping their
/// order within a group: rows computed only for the page take their
/// group's place, and rows not computed come last.
pub fn group_order(items: &mut [(MergeRequest, Option<Readiness>)]) {
    items.sort_by_key(|(_, r)| match r {
        Some(r) => (false, group_rank(&r.tone), r.headline.clone()),
        None => (true, 0, String::new()),
    });
}

impl Services {
    /// A page of the merge request list as the web UI shows it, in a
    /// view (personal ones need a signed-in subject). Open lists
    /// are grouped by readiness: outdated snapshots of the matching merge
    /// requests are computed first (at most
    /// `limits.mr_list_readiness_rebuilds`), so the order and the groups'
    /// counts come from current ones. The caller puts the rows it shows
    /// in [`group_order`]. `(offset, limit)` choose the rows.
    pub async fn mr_list_page(
        &self,
        subject: &Subject,
        ns: &str,
        repo: &str,
        filter: &crate::labels::FilterInput,
        view: View,
        (offset, limit): (i64, i64),
    ) -> Result<ListPage> {
        use slath_db::merge_requests as mrs;
        let (a, f) = self.mr_list_filter(subject, ns, repo, filter).await?;
        let mut o = view.options(subject)?;
        o.grouped = f.state.as_deref() == Some("open");
        if o.grouped || o.ready.is_some() {
            self.refresh_readiness(&a, &f, &o).await?;
        }
        let ms = mrs::list(&self.pool, a.repo.id, &f, &o, offset, limit).await?;
        let readiness = self.mr_list_readiness(&a, &ms).await?;
        let items = ms.into_iter().zip(readiness).collect();
        let counts = mrs::counts(&self.pool, a.repo.id, &f, &o).await?;
        let groups = if o.grouped {
            Some(mrs::readiness_groups(&self.pool, a.repo.id, &f, &o).await?)
        } else {
            None
        };
        Ok(ListPage {
            access: a,
            items,
            counts,
            groups,
        })
    }

    /// Computes the outdated snapshots of the open merge requests matching
    /// `f` again, a few at a time, at most
    /// `limits.mr_list_readiness_rebuilds` of them.
    async fn refresh_readiness(
        &self,
        a: &RepoAccess,
        f: &slath_db::issues::Filter,
        o: &slath_db::merge_requests::ListOptions,
    ) -> Result<()> {
        use futures_util::StreamExt;
        let most = i64::try_from(self.config.limits.mr_list_readiness_rebuilds).unwrap_or(i64::MAX);
        if most == 0 {
            return Ok(());
        }
        let ids =
            slath_db::merge_requests::outdated_readiness(&self.pool, a.repo.id, f, o, most).await?;
        if ids.is_empty() {
            return Ok(());
        }
        let snaps = db::get_many(&self.pool, &ids).await?;
        futures_util::stream::iter(snaps)
            .for_each_concurrent(8, |s| async move {
                if let Err(e) = self.rebuild_readiness(a, s.mr_id, Some(s.generation)).await {
                    tracing::warn!(error = %e, mr = %s.mr_id, "computing a merge request's readiness failed");
                }
            })
            .await;
        Ok(())
    }

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
