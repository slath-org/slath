// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! The one-maintainer policy: with the protection setting on and exactly
//! one person with write access, that person's own review of the exact
//! head counts where only another person's approval could otherwise. The
//! fact is computed from the grants on every evaluation, so a second
//! writer ends the policy at once.

use slath_core::authz::Role;
use slath_core::ids::UserId;
use slath_db::merge_requests::MergeRequest;
use slath_db::mr_reviews::Review;
use slath_db::repos::Repo;

use crate::Services;
use crate::error::{Result, ServiceError};
use crate::mr_reviews::{ApprovalCoverage, Requirement, met, unmet};
use crate::protections::Effective;
use crate::repos::RepoAccess;

/// Approvals nobody can give: fewer people may approve than are needed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stuck {
    /// The author is the only person with write access and the
    /// one-maintainer setting is off: turning it on unblocks them.
    pub policy_helps: bool,
}

/// Refusal of the maintainer's review to anyone else.
pub const ONLY_MAINTAINER: &str =
    "Only the repository's one maintainer can record a review under the one-maintainer policy.";

/// The maintainer's review that counts under the policy.
#[derive(Debug, Clone)]
pub struct PolicyReview {
    /// The maintainer's name.
    pub name: String,
    /// Their review of the exact head, with its coverage; `None` without one.
    pub review: Option<(Review, ApprovalCoverage)>,
}

impl PolicyReview {
    /// Whether the review viewed every file of the diff.
    #[must_use]
    pub fn complete(&self) -> bool {
        self.review
            .as_ref()
            .is_some_and(|(_, c)| c.files_viewed >= c.files_total)
    }

    /// The approvals requirement under the policy, from `approvals` as
    /// the normal rules have it; whether the policy met it.
    #[must_use]
    pub fn approvals(&self, mut approvals: Requirement) -> (Requirement, bool) {
        if approvals.state == "met" {
            return (approvals, false);
        }
        match &self.review {
            Some((_, c)) => (
                met(
                    "approvals",
                    format!(
                        "Reviewed by {} under the one-maintainer policy ({}).",
                        self.name,
                        c.describe()
                    ),
                ),
                true,
            ),
            None => {
                approvals.reason.push_str(&format!(
                    " Under the one-maintainer policy, {}'s review of this revision counts.",
                    self.name
                ));
                approvals.fix = Some("Review it".to_owned());
                (approvals, false)
            }
        }
    }

    /// The sensitive-path requirement under the policy for the paths
    /// `list`, when no maintainer's approval meets it; whether the policy
    /// met it.
    #[must_use]
    pub fn sensitive(&self, list: &str) -> (Requirement, bool) {
        match &self.review {
            Some(_) if self.complete() => (
                met(
                    "sensitive_paths",
                    format!(
                        "Changes sensitive paths ({list}); reviewed by {} under the one-maintainer policy with every file viewed.",
                        self.name
                    ),
                ),
                true,
            ),
            Some((_, c)) => (
                unmet(
                    "sensitive_paths",
                    format!(
                        "Changes sensitive paths: {list}. Under the one-maintainer policy, {}'s review needs every file viewed: {} of {} are.",
                        self.name, c.files_viewed, c.files_total
                    ),
                    "Review every file",
                ),
                false,
            ),
            None => (
                unmet(
                    "sensitive_paths",
                    format!(
                        "Changes sensitive paths: {list}. Under the one-maintainer policy, {}'s review of this revision with every file viewed counts.",
                        self.name
                    ),
                    "Review it",
                ),
                false,
            ),
        }
    }
}

/// The policy as it stands for one protection and repository.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OneMaintainer {
    /// The only person with write access, when there is exactly one.
    pub maintainer: Option<(UserId, String)>,
    /// The setting is on and the fact holds.
    pub applies: bool,
}

impl Services {
    /// The only active person with write access or higher on the
    /// repository by their grants; `None` with none or several. Bots and
    /// instance admins without a grant don't count.
    pub(crate) async fn sole_maintainer(&self, repo: &Repo) -> Result<Option<UserId>> {
        let w = self.writers(repo, 2).await?;
        Ok((w.len() == 1).then(|| w[0]))
    }

    /// The active people with write access or higher by their grants, in
    /// a stable order; at most `most` of them (a cap keeps large
    /// organizations cheap).
    pub(crate) async fn writers(&self, repo: &Repo, most: usize) -> Result<Vec<UserId>> {
        let mut out = Vec::new();
        for u in slath_db::repos::writer_candidates(&self.pool, repo.id).await? {
            if out.len() >= most {
                break;
            }
            if self
                .granted_role(repo, u)
                .await?
                .is_some_and(|r| r >= Role::Write)
            {
                out.push(u);
            }
        }
        Ok(out)
    }

    /// The names of up to three people with write access, for "the policy
    /// doesn't apply" ("alice and bob", "alice, bob, and others").
    pub(crate) async fn writer_names(&self, repo: &Repo) -> Result<String> {
        let ids = self.writers(repo, 4).await?;
        let mut names = Vec::new();
        for u in ids.iter().take(3) {
            if let Some(x) = slath_db::users::get(&self.pool, *u).await? {
                names.push(x.name);
            }
        }
        Ok(match (names.as_slice(), ids.len() > 3) {
            ([], _) => String::new(),
            ([a], _) => a.clone(),
            ([a, b], false) => format!("{a} and {b}"),
            (list, false) => format!(
                "{}, and {}",
                list[..list.len() - 1].join(", "),
                list[list.len() - 1]
            ),
            (list, true) => format!("{}, and others", list.join(", ")),
        })
    }

    /// The one-maintainer policy for a protection; `None` without the
    /// setting.
    pub(crate) async fn one_maintainer(
        &self,
        repo: &Repo,
        eff: &Effective,
    ) -> Result<Option<OneMaintainer>> {
        if !eff.single_maintainer {
            return Ok(None);
        }
        let Some(u) = self.sole_maintainer(repo).await? else {
            return Ok(Some(OneMaintainer {
                maintainer: None,
                applies: false,
            }));
        };
        let name = slath_db::users::get(&self.pool, u)
            .await?
            .map(|x| x.name)
            .unwrap_or_default();
        Ok(Some(OneMaintainer {
            maintainer: Some((u, name)),
            applies: true,
        }))
    }

    /// Refuses the maintainer's review unless the subject is the one
    /// maintainer while the policy applies, for the whole merge request.
    pub(crate) async fn check_one_maintainer(
        &self,
        a: &RepoAccess,
        m: &MergeRequest,
        me: UserId,
        change: Option<&str>,
    ) -> Result<()> {
        if change.is_some_and(|c| !c.trim().is_empty()) {
            return Err(ServiceError::invalid(
                "change",
                "accountable_review",
                "A review under the one-maintainer policy covers the whole merge request.",
            ));
        }
        let eff = self.effective_for(m).await?;
        match self.one_maintainer(&a.repo, &eff).await? {
            Some(OneMaintainer {
                maintainer: Some((u, _)),
                applies: true,
            }) if u == me => Ok(()),
            _ => Err(ServiceError::invalid(
                "verdict",
                "accountable_review",
                ONLY_MAINTAINER,
            )),
        }
    }

    /// The maintainer's review under the policy: their latest
    /// `accountable_review`, if it was given on the current head. `None`
    /// when the policy doesn't apply.
    pub(crate) async fn policy_review(
        &self,
        a: &RepoAccess,
        m: &MergeRequest,
        eff: &Effective,
        reviews: &[Review],
    ) -> Result<Option<PolicyReview>> {
        let Some(OneMaintainer {
            maintainer: Some((u, name)),
            applies: true,
        }) = self.one_maintainer(&a.repo, eff).await?
        else {
            return Ok(None);
        };
        let latest = reviews
            .iter()
            .rev()
            .find(|r| r.reviewer_id == Some(u) && r.verdict == "accountable_review");
        let Some(r) = latest else {
            return Ok(Some(PolicyReview { name, review: None }));
        };
        let revs = slath_db::merge_requests::revisions(&self.pool, m.id).await?;
        let head_of = |seq: i32| {
            revs.iter()
                .find(|v| v.seq == seq)
                .map(|v| v.head_oid.as_str())
        };
        if head_of(r.revision_seq) != m.head_oid.as_deref() {
            return Ok(Some(PolicyReview { name, review: None }));
        }
        let coverage = self
            .approval_coverage(m, &[(r, name.clone())])
            .await?
            .and_then(|c| c.approvals.into_iter().next());
        Ok(Some(PolicyReview {
            review: coverage.map(|c| (r.clone(), c)),
            name,
        }))
    }

    /// Records in the merge's transaction that the one-maintainer policy
    /// met `by_policy` (nothing when it met none).
    pub(crate) async fn record_policy(
        tx: &mut sqlx::PgConnection,
        m: &MergeRequest,
        user: UserId,
        by_policy: &[&'static str],
    ) -> Result<()> {
        if by_policy.is_empty() {
            return Ok(());
        }
        slath_db::mr_ledger::append(
            &mut *tx,
            m.id,
            &slath_db::mr_ledger::NewEntry {
                revision_seq: Some(m.revisions),
                kind: "one_maintainer_policy",
                reason: Some("one_maintainer"),
                detail: serde_json::json!({"maintainer": user.as_uuid(), "met": by_policy}),
                actor: Some(user),
                ..Default::default()
            },
        )
        .await?;
        Ok(())
    }

    /// Explains an unmet approvals requirement that people can't meet by
    /// asking for a review: the one-maintainer setting is on but doesn't
    /// apply, or fewer people may approve than are needed. Whether
    /// approvals are stuck.
    pub(crate) async fn explain_approvals(
        &self,
        a: &RepoAccess,
        m: &MergeRequest,
        eff: &Effective,
        approvals: &mut Requirement,
    ) -> Result<Option<Stuck>> {
        if approvals.state == "met" {
            return Ok(None);
        }
        let policy = self.one_maintainer(&a.repo, eff).await?;
        if policy.as_ref().is_some_and(|p| p.applies) {
            return Ok(None);
        }
        if policy.is_some() {
            let names = self.writer_names(&a.repo).await?;
            approvals.reason.push_str(&if names.is_empty() {
                " The one-maintainer policy doesn't apply: nobody has write access.".to_owned()
            } else {
                format!(" The one-maintainer policy doesn't apply: {names} have write access.")
            });
        }
        // Bots may approve here: whether enough of them can isn't known.
        if eff.bots_count_as_approvers {
            return Ok(None);
        }
        let needed = usize::try_from(eff.required_approvals).unwrap_or(0);
        let writers = self.writers(&a.repo, needed + 1).await?;
        let author_writes = m.author_id.is_some_and(|u| writers.contains(&u));
        let possible = writers.len() - usize::from(author_writes);
        if possible >= needed {
            return Ok(None);
        }
        let policy_helps = policy.is_none() && author_writes && writers.len() == 1;
        approvals.reason.push_str(&match possible {
            0 => " Nobody else with write access can approve it.".to_owned(),
            1 => " Only 1 other person with write access can approve it.".to_owned(),
            n => format!(" Only {n} other people with write access can approve it."),
        });
        approvals.fix = Some(if policy_helps {
            "Turn on the one-maintainer policy in branch protection, or give someone else write access"
                .to_owned()
        } else {
            "Give more people write access, or lower the required approvals in branch protection"
                .to_owned()
        });
        Ok(Some(Stuck { policy_helps }))
    }

    /// Whether a protection rule of the repository has the one-maintainer
    /// policy and it applies now (exactly one person has write access).
    pub async fn one_maintainer_in_effect(&self, a: &RepoAccess) -> Result<bool> {
        let rules = slath_db::protections::list(&self.pool, a.repo.id).await?;
        Ok(rules.iter().any(|r| r.single_maintainer)
            && self.sole_maintainer(&a.repo).await?.is_some())
    }

    /// The only person with write access by name, and what in the rules
    /// can never be met on their merge requests: approvals nobody else can
    /// give, and required checks only they (or their bots) may report.
    pub async fn protection_dead_ends(
        &self,
        a: &RepoAccess,
        rules: &[slath_db::protections::Protection],
    ) -> Result<(Option<String>, Vec<String>)> {
        let Some(u) = self.sole_maintainer(&a.repo).await? else {
            return Ok((None, Vec::new()));
        };
        let name = slath_db::users::get(&self.pool, u)
            .await?
            .map(|x| x.name)
            .unwrap_or_default();
        let mut out = Vec::new();
        for r in rules {
            if r.required_approvals > 0 && !r.single_maintainer && !r.bots_count_as_approvers {
                out.push(format!(
                    "{}: only {name} has write access, so nobody else can approve {name}'s merge requests. Turn on \u{201c}Only one maintainer: their own review counts\u{201d} for this rule, or set its approvals to 0.",
                    r.pattern
                ));
            }
            let lists = crate::protections::reporters_of(r);
            for ctx in &r.required_status_contexts {
                let Some(ids) = lists.get(ctx).filter(|l| !l.is_empty()) else {
                    continue;
                };
                let mut own = true;
                for id in ids {
                    own &= *id == u
                        || slath_db::users::bot_sole_person(&self.pool, *id).await? == Some(u);
                }
                if own {
                    out.push(format!(
                        "{}: only {name} and {name}'s bots may report {ctx}, and their statuses never count on {name}'s own merge requests. Add a reporter that isn't {name}'s, or don't require {ctx}.",
                        r.pattern
                    ));
                }
            }
        }
        Ok((Some(name), out))
    }
}
