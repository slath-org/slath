// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! The review requirements of a merge request, evaluated per change of a
//! prefix of its stack (the whole stack when merging everything): each
//! change needs the required approvals, no change may have a request for
//! changes, and sensitive paths need a maintainer's approval of every
//! change. A reviewer's verdict on the whole revision covers every change;
//! a verdict on one change covers only that one.

use std::collections::{HashMap, HashSet};

use slath_core::authz::{Action, Role, UserKind};
use slath_core::ids::UserId;
use slath_db::merge_requests::MergeRequest;
use slath_db::mr_reviews::{self as db, Review};

use crate::Services;
use crate::error::Result;
use crate::mr_reviews::{
    ChangeVerdicts, Coverage, History, Requirement, Standing, met, plural, standings, unmet,
};
use crate::protections::Effective;
use crate::repos::RepoAccess;

/// One change's standing among the people whose verdicts count, by name.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Judged {
    /// The change's display ID, short; `None` for a revision reviewed as a
    /// whole.
    pub label: Option<String>,
    pub approvers: Vec<String>,
    pub blockers: Vec<String>,
}

/// The `approvals` requirement over `js`. `named` describes an approver
/// of every change (with what they had viewed).
pub(crate) fn approvals_requirement(
    js: &[Judged],
    needed: usize,
    named: &dyn Fn(&str) -> String,
    stale_note: &str,
) -> Requirement {
    let sets: Vec<HashSet<&String>> = js.iter().map(|j| j.approvers.iter().collect()).collect();
    // The same people approve every change: as for a revision as a whole.
    if js.len() <= 1 || sets.windows(2).all(|w| w[0] == w[1]) {
        let names: Vec<String> = js
            .first()
            .map(|j| j.approvers.iter().map(|n| named(n)).collect())
            .unwrap_or_default();
        return if names.len() >= needed {
            met(
                "approvals",
                if names.is_empty() {
                    "No approvals are required.".to_owned()
                } else {
                    format!("Approved by {}.", names.join(", "))
                },
            )
        } else if names.is_empty() {
            unmet(
                "approvals",
                format!("0 of {needed} required approvals{stale_note}."),
                "Ask a reviewer with write access",
            )
        } else {
            unmet(
                "approvals",
                format!(
                    "{} of {needed} required approvals{stale_note}: {}.",
                    names.len(),
                    names.join(", ")
                ),
                "Ask a reviewer with write access",
            )
        };
    }
    let label = |j: &Judged| j.label.clone().unwrap_or_default();
    let short: Vec<String> = js
        .iter()
        .filter(|j| j.approvers.len() < needed)
        .map(|j| format!("{} ({} of {needed})", label(j), j.approvers.len()))
        .collect();
    if short.is_empty() {
        let each: Vec<String> = js
            .iter()
            .filter(|j| !j.approvers.is_empty())
            .map(|j| format!("{} by {}", label(j), j.approvers.join(", ")))
            .collect();
        met(
            "approvals",
            if needed == 0 {
                format!("No approvals are required; approved: {}.", each.join("; "))
            } else {
                format!("Every change is approved: {}.", each.join("; "))
            },
        )
    } else {
        unmet(
            "approvals",
            format!(
                "{} of {} have the required approvals{stale_note}; missing: {}.",
                js.len() - short.len(),
                plural(js.len(), "change", "changes"),
                short.join(", ")
            ),
            "Ask a reviewer with write access to review the missing changes",
        )
    }
}

/// The `changes_requested` requirement over `js`.
pub(crate) fn blockers_requirement(js: &[Judged]) -> Requirement {
    let mut order: Vec<&String> = Vec::new();
    let mut on: HashMap<&String, Vec<String>> = HashMap::new();
    for j in js {
        for b in &j.blockers {
            if !order.contains(&b) {
                order.push(b);
            }
            on.entry(b)
                .or_default()
                .push(j.label.clone().unwrap_or_default());
        }
    }
    if order.is_empty() {
        return met("changes_requested", "No one requested changes.");
    }
    let who: Vec<String> = order
        .iter()
        .map(|b| {
            let labels = &on[b];
            if labels.len() == js.len() {
                (*b).clone()
            } else {
                format!("{b} (on {})", labels.join(", "))
            }
        })
        .collect();
    unmet(
        "changes_requested",
        format!("{} requested changes.", who.join(", ")),
        "Push changes that address the review",
    )
}

/// The review requirements and the coverage of the approvals that count.
pub(crate) struct ReviewRequirements {
    pub approvals: Requirement,
    pub changes_requested: Requirement,
    /// Maintainers (by name) who approve every change, for the
    /// sensitive-path requirement.
    pub maintainers: Vec<String>,
    pub coverage: Option<Coverage>,
}

/// Each standing among the people whose verdicts count, by name.
fn judge(
    all: &[Standing],
    labels: &HashMap<uuid::Uuid, String>,
    eligible: &HashMap<UserId, bool>,
    names: &HashMap<UserId, String>,
) -> Vec<Judged> {
    let ok = |u: &UserId| eligible.get(u).copied().unwrap_or(false);
    let name = |u: &UserId| names.get(u).cloned().unwrap_or_default();
    all.iter()
        .map(|s| Judged {
            label: s.change.and_then(|k| labels.get(&k).cloned()),
            approvers: s
                .approvals
                .iter()
                .filter(|(u, _)| ok(u))
                .map(|(u, _)| name(u))
                .collect(),
            blockers: s.blockers.iter().filter(|u| ok(u)).map(name).collect(),
        })
        .collect()
}

impl Services {
    /// Whose verdicts count, per reviewer of `reviews`: not the author's or
    /// the author's bots', and only people (and bots, if allowed) with
    /// write access; with each reviewer's name.
    async fn eligible_reviewers(
        &self,
        a: &RepoAccess,
        m: &MergeRequest,
        eff: &Effective,
        reviews: &[Review],
    ) -> Result<(HashMap<UserId, bool>, HashMap<UserId, String>)> {
        let mut eligible: HashMap<UserId, bool> = HashMap::new();
        let mut names: HashMap<UserId, String> = HashMap::new();
        for r in reviews {
            let Some(u) = r.reviewer_id else { continue };
            if let Some(n) = &r.reviewer {
                names.insert(u, n.clone());
            }
            if eligible.contains_key(&u) {
                continue;
            }
            let ok = m.author_id != Some(u)
                && !(m.author_id.is_some()
                    && slath_db::users::bot_owner_user(&self.pool, u).await? == m.author_id)
                && r.reviewer.is_some()
                && self
                    .verdict_counts(a, u, eff.bots_count_as_approvers)
                    .await?;
            eligible.insert(u, ok);
        }
        Ok((eligible, names))
    }

    /// The verdicts that count for each change of the current revision,
    /// bottom first: the same as the review requirements count. Empty when
    /// the revision is reviewed as a whole or a commit is unresolved.
    pub async fn mr_change_verdicts(
        &self,
        a: &RepoAccess,
        m: &MergeRequest,
    ) -> Result<Vec<ChangeVerdicts>> {
        let order = self.change_order(m).await?;
        if order.is_empty() {
            return Ok(Vec::new());
        }
        let eff = self.effective_for(m).await?;
        let reviews = db::list(&self.pool, m.id).await?;
        let h: History = self.history(m, m.revisions).await?;
        let all = standings(&reviews, &h, &order);
        let labels: HashMap<uuid::Uuid, String> = slath_db::mr_changes::changes(&self.pool, m.id)
            .await?
            .into_iter()
            .map(|c| (c.id, c.display_id))
            .collect();
        let (eligible, names) = self.eligible_reviewers(a, m, &eff, &reviews).await?;
        Ok(judge(&all, &labels, &eligible, &names)
            .into_iter()
            .map(|j| ChangeVerdicts {
                display_id: j.label.unwrap_or_default(),
                approvers: j.approvers,
                blockers: j.blockers,
            })
            .collect())
    }

    /// The current revision's changes, bottom first; empty when it's
    /// reviewed as a whole or a commit is still unresolved (which blocks
    /// merging anyway, so its verdicts only matter as a whole).
    pub(crate) async fn change_order(&self, m: &MergeRequest) -> Result<Vec<uuid::Uuid>> {
        let mut here: Vec<_> = slath_db::mr_changes::versions(&self.pool, m.id)
            .await?
            .into_iter()
            .filter(|v| v.revision_seq == m.revisions)
            .collect();
        here.sort_by_key(|v| v.position);
        Ok(here
            .iter()
            .map(|v| v.change_id)
            .collect::<Option<Vec<_>>>()
            .unwrap_or_default())
    }

    /// The review requirements for the first `upto` changes of the stack
    /// (`None`: all of it).
    pub(crate) async fn review_requirements(
        &self,
        a: &RepoAccess,
        m: &MergeRequest,
        eff: &Effective,
        upto: Option<usize>,
    ) -> Result<ReviewRequirements> {
        let reviews = db::list(&self.pool, m.id).await?;
        let h: History = self.history(m, m.revisions).await?;
        let mut order = self.change_order(m).await?;
        if let Some(n) = upto {
            order.truncate(n.max(1));
        }
        let all: Vec<Standing> = standings(&reviews, &h, &order);
        let by_id: HashMap<uuid::Uuid, &Review> = reviews.iter().map(|r| (r.id, r)).collect();
        let labels: HashMap<uuid::Uuid, String> = slath_db::mr_changes::changes(&self.pool, m.id)
            .await?
            .into_iter()
            .map(|c| (c.id, c.display_id.chars().take(8).collect()))
            .collect();
        let (eligible, names) = self.eligible_reviewers(a, m, eff, &reviews).await?;
        let ok = |u: &UserId| eligible.get(u).copied().unwrap_or(false);
        let name = |u: &UserId| names.get(u).cloned().unwrap_or_default();
        let judged = judge(&all, &labels, &eligible, &names);
        // Each counting approval once, for coverage.
        let mut seen = HashSet::new();
        let approvals: Vec<(&Review, String)> = all
            .iter()
            .flat_map(|s| s.approvals.iter())
            .filter(|(u, id)| ok(u) && seen.insert(*id))
            .filter_map(|(u, id)| Some((*by_id.get(id)?, name(u))))
            .collect();
        let coverage = self.approval_coverage(m, &approvals).await?;
        let stale = crate::mr_reviews::counting(&reviews, &h)
            .iter()
            .filter(|v| v.stale && v.review.verdict == "approve")
            .filter(|v| v.review.reviewer_id.as_ref().is_some_and(ok))
            .count();
        let stale_note = if stale > 0 {
            format!(
                " ({} stale after later changes)",
                plural(stale, "approval is", "approvals are")
            )
        } else {
            String::new()
        };
        // What an approver had viewed, when one approval of theirs counts
        // (for several changes, each has its own).
        let single = |n: &str| approvals.iter().filter(|(_, m)| m == n).count() == 1;
        let named = |n: &str| match coverage
            .as_ref()
            .filter(|_| single(n))
            .and_then(|c| c.approvals.iter().find(|x| x.reviewer == n))
        {
            Some(x) => format!("{n} ({})", x.describe()),
            None => n.to_owned(),
        };
        let needed = usize::try_from(eff.required_approvals).unwrap_or(0);
        // People with the maintain role who approve every change.
        let mut maintainers = Vec::new();
        if let Some(first) = all.first() {
            for (u, _) in first.approvals.iter().filter(|(u, _)| ok(u)) {
                if !all.iter().all(|s| s.approvals.iter().any(|(v, _)| v == u)) {
                    continue;
                }
                let Some(user) = slath_db::users::get(&self.pool, *u).await? else {
                    continue;
                };
                if user.kind == UserKind::Bot {
                    continue;
                }
                let role = self
                    .repo_access_by_id(
                        &crate::notifications::user_subject(*u, &user),
                        a.repo.id,
                        Action::RepoRead,
                    )
                    .await
                    .ok()
                    .and_then(|x| x.ctx.role);
                if role.is_some_and(|r| r >= Role::Maintain) {
                    maintainers.push(name(u));
                }
            }
        }
        Ok(ReviewRequirements {
            approvals: approvals_requirement(&judged, needed, &named, &stale_note),
            changes_requested: blockers_requirement(&judged),
            maintainers,
            coverage,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn j(label: &str, approvers: &[&str], blockers: &[&str]) -> Judged {
        Judged {
            label: Some(label.to_owned()),
            approvers: approvers.iter().map(|s| (*s).to_owned()).collect(),
            blockers: blockers.iter().map(|s| (*s).to_owned()).collect(),
        }
    }

    fn plain(n: &str) -> String {
        n.to_owned()
    }

    #[test]
    fn each_change_needs_its_approvals() {
        // The same approvers everywhere read as before.
        let r = approvals_requirement(
            &[j("a", &["bob"], &[]), j("b", &["bob"], &[])],
            1,
            &plain,
            "",
        );
        assert_eq!((r.state, r.reason.as_str()), ("met", "Approved by bob."));
        // One change short.
        let r = approvals_requirement(&[j("a", &["bob"], &[]), j("b", &[], &[])], 1, &plain, "");
        assert_eq!(r.state, "unmet");
        assert_eq!(
            r.reason,
            "1 of 2 changes have the required approvals; missing: b (0 of 1)."
        );
        // Different people, but enough for each.
        let r = approvals_requirement(
            &[j("a", &["bob"], &[]), j("b", &["carol"], &[])],
            1,
            &plain,
            "",
        );
        assert_eq!(r.state, "met");
        assert_eq!(r.reason, "Every change is approved: a by bob; b by carol.");
        // Nothing required.
        let r = approvals_requirement(&[j("a", &[], &[])], 0, &plain, "");
        assert_eq!(r.state, "met");
        let r = approvals_requirement(&[j("a", &["bob"], &[]), j("b", &[], &[])], 0, &plain, "");
        assert_eq!(r.reason, "No approvals are required; approved: a by bob.");
    }

    #[test]
    fn a_request_for_changes_on_any_change_blocks() {
        assert_eq!(
            blockers_requirement(&[j("a", &[], &[]), j("b", &[], &[])]).state,
            "met"
        );
        let r = blockers_requirement(&[j("a", &[], &["bob"]), j("b", &[], &["bob", "carol"])]);
        assert_eq!(r.state, "unmet");
        assert_eq!(r.reason, "bob, carol (on b) requested changes.");
    }
}
