// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Reviews of merge requests: verdicts on a revision, reviewer requests
//! (users and teams) with the "review requests" inbox, which approvals
//! still count after new revisions, and the requirements a merge request
//! must meet before it can be merged (spec: verdicts and requirements).

use std::collections::HashMap;

use slath_core::authz::{Action, Subject, UserKind};
use slath_core::ids::{MergeRequestId, UserId};
use slath_db::merge_requests::MergeRequest;
use slath_db::mr_reviews::{self as db, Review};

use crate::Services;
use crate::error::{Result, ServiceError};
use crate::protections::Effective;
use crate::repos::RepoAccess;

/// What a review says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Approve,
    RequestChanges,
    Comment,
}

impl Verdict {
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "approve" => Some(Self::Approve),
            "request_changes" => Some(Self::RequestChanges),
            "comment" => Some(Self::Comment),
            _ => None,
        }
    }

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Approve => "approve",
            Self::RequestChanges => "request_changes",
            Self::Comment => "comment",
        }
    }
}

/// A review with whether it still counts.
#[derive(Debug, Clone)]
pub struct ReviewView {
    pub review: Review,
    /// The reviewer's latest approval or request for changes, and current:
    /// on this revision, carried over pure rebases, or never dismissed.
    pub counts: bool,
    /// An approval or request for changes made stale by later changes.
    pub stale: bool,
}

/// One requirement of the merge box.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Requirement {
    /// `open`, `draft`, `approvals`, `changes_requested`, `up_to_date`,
    /// `statuses` (one per required check), `threads`, `conflicts`, or
    /// `identity`.
    pub key: &'static str,
    /// `met`, `unmet`, or `pending`.
    pub state: &'static str,
    /// How the merge box shows it: `done`, `waiting` (normal, nothing is
    /// wrong yet: a draft, missing approvals, checks running), or
    /// `blocking` (something must change).
    pub tone: &'static str,
    pub reason: String,
    /// What fixes it, in words (e.g. "Mark as ready").
    pub fix: Option<String>,
}

/// Whether a merge request can be merged, and why not.
#[derive(Debug, Clone, Default)]
pub struct Mergeability {
    pub mergeable: bool,
    pub requirements: Vec<Requirement>,
}

fn met(key: &'static str, reason: impl Into<String>) -> Requirement {
    Requirement {
        key,
        state: "met",
        tone: "done",
        reason: reason.into(),
        fix: None,
    }
}

fn unmet(key: &'static str, reason: impl Into<String>, fix: &str) -> Requirement {
    Requirement {
        key,
        state: "unmet",
        tone: if matches!(key, "draft" | "approvals") {
            "waiting"
        } else {
            "blocking"
        },
        reason: reason.into(),
        fix: Some(fix.to_owned()),
    }
}

fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

/// When a timeline entry happened, and who did it (by name).
#[must_use]
pub fn item_time(
    item: &crate::merge_requests::MrTimelineItem,
) -> Option<(time::OffsetDateTime, Option<String>)> {
    use crate::merge_requests::MrTimelineItem as I;
    match item {
        I::Comment(crate::issues::TimelineItem::Comment { comment, .. }) => {
            Some((comment.created_at, comment.author.clone()))
        }
        I::Comment(crate::issues::TimelineItem::Event(e, _)) | I::Event(e) => {
            Some((e.created_at, e.actor.clone()))
        }
        I::Revision { revision, .. } => Some((revision.created_at, revision.pushed_by.clone())),
        I::Review(v) => Some((v.review.created_at, v.review.reviewer.clone())),
        I::Thread(t) => Some((t.thread.created_at, t.thread.author.clone())),
    }
}

/// Which reviews count on revision `current`: each reviewer's latest
/// approval or request for changes, if it was made on `current`, or every
/// revision since kept the same patches, or stale approvals aren't
/// dismissed.
#[must_use]
pub fn counting(
    reviews: &[Review],
    current: i32,
    same_patches: &HashMap<i32, bool>,
    dismiss_stale: bool,
) -> Vec<ReviewView> {
    counting_with(reviews, current, same_patches, dismiss_stale, false)
}

/// Like [`counting`]; with `strict`, any later revision makes approvals
/// and requests for changes stale.
#[must_use]
pub fn counting_with(
    reviews: &[Review],
    current: i32,
    same_patches: &HashMap<i32, bool>,
    dismiss_stale: bool,
    strict: bool,
) -> Vec<ReviewView> {
    let mut latest: HashMap<UserId, uuid::Uuid> = HashMap::new();
    for r in reviews.iter().filter(|r| r.verdict != "comment") {
        if let Some(u) = r.reviewer_id {
            latest.insert(u, r.id);
        }
    }
    let carried = |seq: i32| {
        if strict {
            return seq == current;
        }
        !dismiss_stale
            || ((seq + 1)..=current).all(|s| same_patches.get(&s).copied().unwrap_or(false))
    };
    reviews
        .iter()
        .map(|r| {
            let decisive =
                r.verdict != "comment" && r.reviewer_id.and_then(|u| latest.get(&u)) == Some(&r.id);
            let current = decisive && carried(r.revision_seq);
            ReviewView {
                review: r.clone(),
                counts: current,
                stale: decisive && !current,
            }
        })
        .collect()
}

impl Services {
    async fn effective_for(&self, m: &MergeRequest) -> Result<Effective> {
        let rules = slath_db::protections::list(&self.pool, m.repo_id).await?;
        Ok(crate::protections::effective(
            &rules,
            m.target_branch.as_bytes(),
        ))
    }

    /// Submits a review of the current revision. The author may only
    /// comment; a comment needs a body.
    pub async fn submit_review(
        &self,
        subject: &Subject,
        (ns, repo, number): (&str, &str, i32),
        verdict: &str,
        body: &str,
    ) -> Result<Review> {
        let (a, m) = self.mr(subject, ns, repo, number).await?;
        if !a.can(subject, Action::MrReview) {
            return Err(ServiceError::Forbidden);
        }
        self.require_confirmed(subject).await?;
        let Subject::User { id: me, .. } = subject else {
            return Err(ServiceError::Unauthenticated);
        };
        if m.state != "open" || m.revisions == 0 {
            return Err(ServiceError::Conflict("merge request isn't open".into()));
        }
        let verdict = Verdict::parse(verdict).ok_or_else(|| {
            ServiceError::invalid(
                "verdict",
                "invalid",
                "Choose comment, approve, or request changes.",
            )
        })?;
        if verdict != Verdict::Comment && m.author_id == Some(*me) {
            return Err(ServiceError::invalid(
                "verdict",
                "own",
                "You can't approve or request changes on your own merge request.",
            ));
        }
        let pending = self.pending_comments(subject, &m).await?;
        if verdict == Verdict::Comment && body.trim().is_empty() && pending == 0 {
            return Err(ServiceError::invalid(
                "body",
                "required",
                "Write a comment.",
            ));
        }
        self.check_body("body", body)?;
        let mut tx = self.pool.begin().await.map_err(ServiceError::internal)?;
        db::insert(
            &mut *tx,
            m.id,
            m.revisions,
            *me,
            verdict.as_str(),
            body.trim(),
        )
        .await?;
        // The review publishes the reviewer's pending comments with it.
        slath_db::mr_threads::publish_pending(&mut *tx, m.id, *me).await?;
        db::mark_reviewed(&mut *tx, m.id, *me).await?;
        slath_db::merge_requests::touch(&mut *tx, m.id).await?;
        tx.commit().await.map_err(ServiceError::internal)?;
        db::list(&self.pool, m.id)
            .await?
            .pop()
            .ok_or(ServiceError::NotFound)
    }

    /// The reviews, with whether each still counts.
    pub async fn mr_reviews(&self, m: &MergeRequest) -> Result<Vec<ReviewView>> {
        let reviews = db::list(&self.pool, m.id).await?;
        let same: HashMap<i32, bool> = db::same_patches(&self.pool, m.id)
            .await?
            .into_iter()
            .map(|(s, v)| (s, v.unwrap_or(false)))
            .collect();
        let eff = self.effective_for(m).await?;
        Ok(counting_with(
            &reviews,
            m.revisions,
            &same,
            eff.dismiss_stale_approvals,
            eff.dismiss_approvals_on_new_revision,
        ))
    }

    /// The viewer's latest review's revision, if older than the current one
    /// ("changes since your last review").
    pub async fn last_reviewed_revision(
        &self,
        subject: &Subject,
        m: &MergeRequest,
    ) -> Result<Option<i32>> {
        let Subject::User { id, .. } = subject else {
            return Ok(None);
        };
        Ok(db::list(&self.pool, m.id)
            .await?
            .iter()
            .rev()
            .find(|r| r.reviewer_id == Some(*id))
            .map(|r| r.revision_seq)
            .filter(|s| *s < m.revisions))
    }

    /// When the viewer last reviewed, and how many timeline entries by
    /// others came after it ("new since your review").
    pub async fn since_last_review(
        &self,
        subject: &Subject,
        a: &RepoAccess,
        m: &MergeRequest,
    ) -> Result<Option<(time::OffsetDateTime, usize)>> {
        let Subject::User { id, .. } = subject else {
            return Ok(None);
        };
        let Some(last) = db::list(&self.pool, m.id)
            .await?
            .iter()
            .rev()
            .find(|r| r.reviewer_id == Some(*id))
            .map(|r| r.created_at)
        else {
            return Ok(None);
        };
        let me = slath_db::users::get(&self.pool, *id).await?.map(|u| u.name);
        let mut count = 0;
        for item in self.mr_timeline(subject, a, m).await? {
            if let Some((t, by)) = item_time(&item)
                && t > last
                && by != me
            {
                count += 1;
            }
        }
        Ok(Some((last, count)))
    }

    /// Sets the requested reviewers (users, or `org/team` for teams of the
    /// repository's organization). The author or triagers; on the
    /// timeline.
    pub async fn set_mr_reviewers(
        &self,
        subject: &Subject,
        (ns, repo, number): (&str, &str, i32),
        names: &[String],
    ) -> Result<()> {
        let (a, m) = self.mr(subject, ns, repo, number).await?;
        if !self.mr_perms(subject, &a, &m).edit {
            return Err(ServiceError::Forbidden);
        }
        let Subject::User { id: me, .. } = subject else {
            return Err(ServiceError::Unauthenticated);
        };
        let (team_names, user_names): (Vec<String>, Vec<String>) = names
            .iter()
            .map(|n| n.trim().trim_start_matches('@').to_owned())
            .filter(|n| !n.is_empty())
            .partition(|n| n.contains('/'));
        let users: Vec<UserId> = self
            .readers(&a, "reviewers", &user_names)
            .await?
            .into_iter()
            .filter(|u| m.author_id != Some(*u))
            .collect();
        let mut teams = Vec::new();
        for t in &team_names {
            let found = match t.split_once('/') {
                Some((org, team)) if org.eq_ignore_ascii_case(&a.repo.namespace) => {
                    match slath_db::orgs::resolve_namespace(&self.pool, org).await? {
                        Some((_, _, slath_db::orgs::NamespaceKind::Org(o))) => {
                            slath_db::teams::find(&self.pool, o, team).await?
                        }
                        _ => None,
                    }
                }
                _ => None,
            };
            let team = found.ok_or_else(|| {
                ServiceError::invalid(
                    "reviewers",
                    "unknown",
                    format!("{t} isn't a team of the organization {}.", a.repo.namespace),
                )
            })?;
            teams.push((team.id, t.clone()));
        }
        let before = db::requested(&self.pool, m.id).await?;
        let mut tx = self.pool.begin().await.map_err(ServiceError::internal)?;
        let event = |kind: &'static str, who: String| (kind, serde_json::json!({"user": who}));
        let mut events = Vec::new();
        for u in &users {
            if !before.iter().any(|b| b.user == Some(*u)) {
                slath_db::merge_requests::add_reviewer(&mut *tx, m.id, *u, *me).await?;
                let name = slath_db::users::get(&self.pool, *u)
                    .await?
                    .map(|x| x.name)
                    .unwrap_or_default();
                events.push(event("review_requested", name));
            }
        }
        for (t, name) in &teams {
            if !before.iter().any(|b| b.team == Some(*t)) {
                db::request_team(&mut *tx, m.id, *t, *me).await?;
                events.push(event("review_requested", name.clone()));
            }
        }
        for b in &before {
            let keep = b.user.is_some_and(|u| users.contains(&u))
                || b.team.is_some_and(|t| teams.iter().any(|(x, _)| *x == t));
            if !keep {
                db::remove_request(&mut *tx, m.id, b.user, b.team).await?;
                events.push(event("review_request_removed", b.name.clone()));
            }
        }
        for (kind, data) in events {
            slath_db::issues::insert_event(
                &mut *tx,
                a.repo.id,
                "merge_request",
                *m.id.as_uuid(),
                Some(*me),
                kind,
                &data,
            )
            .await?;
        }
        tx.commit().await.map_err(ServiceError::internal)?;
        Ok(())
    }

    /// Open merge requests waiting for the subject's review, that the
    /// subject can still read.
    pub async fn review_requests(&self, subject: &Subject) -> Result<Vec<db::Waiting>> {
        let Subject::User { id, .. } = subject else {
            return Err(ServiceError::Unauthenticated);
        };
        let mut out = Vec::new();
        for w in db::waiting_for(&self.pool, *id, 200).await? {
            if self
                .repo_access_by_id(subject, w.repo, Action::RepoRead)
                .await
                .is_ok()
            {
                out.push(w);
            }
        }
        Ok(out)
    }

    /// Whether a user's verdict counts: write access to the repository,
    /// not a bot unless the rules let bots approve.
    async fn verdict_counts(&self, a: &RepoAccess, user: UserId, bots: bool) -> Result<bool> {
        let Some(u) = slath_db::users::get(&self.pool, user).await? else {
            return Ok(false);
        };
        if u.kind == UserKind::Bot && !bots {
            return Ok(false);
        }
        Ok(self
            .repo_access_by_id(
                &crate::notifications::user_subject(user, &u),
                a.repo.id,
                Action::RepoPush,
            )
            .await
            .is_ok())
    }

    /// The requirements of the merge box for the current revision.
    pub async fn mr_mergeability(&self, a: &RepoAccess, m: &MergeRequest) -> Result<Mergeability> {
        let eff = self.effective_for(m).await?;
        let mut reqs = Vec::new();
        reqs.push(match m.state.as_str() {
            "open" => met("open", "The merge request is open."),
            "merged" => unmet("open", "It's already merged.", "Nothing to do"),
            _ => unmet("open", "It's closed.", "Reopen it"),
        });
        reqs.push(if m.draft {
            unmet("draft", "It's a draft.", "Mark it as ready")
        } else {
            met("draft", "It's ready for review.")
        });
        let views = self.mr_reviews(m).await?;
        let mut approvers = Vec::new();
        let mut blockers = Vec::new();
        let mut stale = 0;
        for v in &views {
            let (Some(u), Some(name)) = (v.review.reviewer_id, v.review.reviewer.clone()) else {
                continue;
            };
            if m.author_id == Some(u)
                || !self
                    .verdict_counts(a, u, eff.bots_count_as_approvers)
                    .await?
            {
                continue;
            }
            match (v.review.verdict.as_str(), v.counts) {
                ("approve", true) => approvers.push(name),
                ("request_changes", true) => blockers.push(name),
                ("approve", false) if v.stale => stale += 1,
                _ => {}
            }
        }
        let needed = usize::try_from(eff.required_approvals).unwrap_or(0);
        let stale_note = if stale > 0 {
            format!(
                " ({} stale after later changes)",
                plural(stale, "approval is", "approvals are")
            )
        } else {
            String::new()
        };
        reqs.push(if approvers.len() >= needed {
            met(
                "approvals",
                if needed == 0 {
                    "No approvals are required.".to_owned()
                } else {
                    format!("Approved by {}.", approvers.join(", "))
                },
            )
        } else {
            unmet(
                "approvals",
                format!(
                    "{} of {needed} required approvals{stale_note}.",
                    approvers.len()
                ),
                "Ask a reviewer with write access",
            )
        });
        reqs.push(if blockers.is_empty() {
            met("changes_requested", "No one requested changes.")
        } else {
            unmet(
                "changes_requested",
                format!("{} requested changes.", blockers.join(", ")),
                "Push changes that address the review",
            )
        });
        let (up_to_date, up_req) = self.up_to_date(m).await?;
        if eff.require_up_to_date {
            reqs.push(up_req);
        }
        reqs.extend(self.check_requirements(m, &eff, up_to_date).await?);
        if eff.require_resolved_threads {
            let open = self.unresolved_threads(m).await?;
            reqs.push(if open == 0 {
                met("threads", "No threads are unresolved.")
            } else {
                unmet(
                    "threads",
                    format!("{} unresolved.", plural(open, "thread is", "threads are")),
                    "Resolve them, or answer and let their authors resolve them",
                )
            });
        }
        if m.state == "open" {
            let files = self.mr_conflicts(m).await?;
            reqs.push(if files.is_empty() {
                met(
                    "conflicts",
                    format!("No conflicts with {}.", m.target_branch),
                )
            } else {
                unmet(
                    "conflicts",
                    format!(
                        "Conflicts with {} in {}: {}.",
                        m.target_branch,
                        plural(files.len(), "file", "files"),
                        files.join(", ")
                    ),
                    "Rebase onto the target and resolve them",
                )
            });
        }
        let stack = self.mr_stack(m).await?;
        let unresolved = stack
            .entries
            .iter()
            .filter(|e| e.binding == "unresolved")
            .count();
        let conflicted = stack.entries.iter().filter(|e| e.conflicted).count();
        reqs.push(if unresolved > 0 {
            unmet(
                "identity",
                format!(
                    "{} with IDs that name more than one change.",
                    plural(unresolved, "commit", "commits")
                ),
                "Choose a change for each on the Commits tab",
            )
        } else if conflicted > 0 {
            unmet(
                "identity",
                format!(
                    "{} with a conflict recorded by jj.",
                    plural(conflicted, "commit", "commits")
                ),
                "Resolve the conflict and push again",
            )
        } else {
            met("identity", "Every commit belongs to one change.")
        });
        Ok(Mergeability {
            mergeable: reqs.iter().all(|r| r.state == "met"),
            requirements: reqs,
        })
    }

    /// Whether the target head is an ancestor of the source head, and the
    /// requirement saying so.
    async fn up_to_date(&self, m: &MergeRequest) -> Result<(bool, Requirement)> {
        let target = slath_git::args::RefName::branch(m.target_branch.as_bytes())
            .map_err(|_| ServiceError::internal("bad target"))?;
        let dir = self.repo_path(m.repo_id);
        let (Some(t), Some(h)) = (
            slath_git::refs::read(&self.runner, &dir, &target)
                .await
                .map_err(ServiceError::internal)?,
            m.head_oid
                .as_deref()
                .and_then(|h| slath_git::ObjectId::parse(h).ok()),
        ) else {
            return Ok((
                false,
                unmet(
                    "up_to_date",
                    "The target branch doesn't exist.",
                    "Choose another target",
                ),
            ));
        };
        let base = slath_git::browse::merge_base(&self.runner, &dir, &t, &h)
            .await
            .map_err(ServiceError::internal)?;
        let ok = base.as_ref() == Some(&t);
        let req = if ok {
            met(
                "up_to_date",
                format!("It's up to date with {}.", m.target_branch),
            )
        } else {
            unmet(
                "up_to_date",
                format!("{} has commits the branch doesn't.", m.target_branch),
                "Rebase or merge the target into the branch",
            )
        };
        Ok((ok, req))
    }

    /// After revision `seq`: whether it kept every change's patch, and the
    /// timeline note on the approvals made before it.
    pub(crate) async fn note_revision(&self, id: MergeRequestId, seq: i32) -> Result<()> {
        let versions = slath_db::mr_changes::versions(&self.pool, id).await?;
        let here: Vec<_> = versions.iter().filter(|v| v.revision_seq == seq).collect();
        let prev = versions
            .iter()
            .map(|v| v.revision_seq)
            .filter(|s| *s < seq)
            .max();
        let same = !here.is_empty()
            && prev.is_some()
            && here
                .iter()
                .all(|v| v.change_id.is_some() && v.patch_id.is_some())
            && {
                let set = |s: i32| {
                    let mut v: Vec<_> = versions
                        .iter()
                        .filter(|v| v.revision_seq == s)
                        .map(|v| (v.change_id, v.patch_id.clone()))
                        .collect();
                    v.sort();
                    v
                };
                prev.is_some_and(|p| set(p) == set(seq))
            };
        db::set_same_patches(&self.pool, id, seq, same).await?;
        let Some(m) = slath_db::merge_requests::get_by_id(&self.pool, id).await? else {
            return Ok(());
        };
        let eff = self.effective_for(&m).await?;
        // The approvals that counted on the previous revision, and what this
        // one does to them.
        let reviews = db::list(&self.pool, id).await?;
        let same_map: HashMap<i32, bool> = db::same_patches(&self.pool, id)
            .await?
            .into_iter()
            .filter(|(s, _)| *s < seq)
            .map(|(s, v)| (s, v.unwrap_or(false)))
            .collect();
        let before = counting_with(
            &reviews,
            seq - 1,
            &same_map,
            eff.dismiss_stale_approvals,
            eff.dismiss_approvals_on_new_revision,
        );
        let stale_reason = if here.is_empty() {
            "whole-only"
        } else if here.iter().any(|v| v.change_id.is_none()) {
            "unresolved"
        } else if here.iter().any(|v| v.state == "inferred") {
            "inferred"
        } else {
            "patch-changed"
        };
        let mut carried_any = false;
        for v in before
            .iter()
            .filter(|v| v.counts && v.review.verdict == "approve")
        {
            let (kind, reason) = match (eff.dismiss_stale_approvals, same) {
                _ if eff.dismiss_approvals_on_new_revision => ("approval_stale", "new-revision"),
                (false, _) => ("approval_carried", "not-dismissed"),
                (true, true) => ("approval_carried", "same-patches"),
                (true, false) => ("approval_stale", stale_reason),
            };
            carried_any = true;
            slath_db::mr_ledger::append(
                &self.pool,
                id,
                &slath_db::mr_ledger::NewEntry {
                    revision_seq: Some(seq),
                    kind,
                    reason: Some(reason),
                    detail: serde_json::json!({
                        "reviewer": v.review.reviewer,
                        "approved_revision": v.review.revision_seq,
                    }),
                    ..Default::default()
                },
            )
            .await?;
        }
        // Statuses on the previous head no longer count.
        let revisions = slath_db::merge_requests::revisions(&self.pool, id).await?;
        if let Some(old) = revisions.iter().find(|r| r.seq == seq - 1) {
            let all = slath_db::statuses::for_commit(&self.pool, m.repo_id, &old.head_oid).await?;
            for st in crate::statuses::latest_per_context(&all) {
                slath_db::mr_ledger::append(
                    &self.pool,
                    id,
                    &slath_db::mr_ledger::NewEntry {
                        revision_seq: Some(seq),
                        kind: "status_stale",
                        state: Some(&st.state),
                        reason: Some("head-moved"),
                        detail: serde_json::json!({
                            "context": st.context,
                            "commit": old.head_oid,
                            "reporter": st.reporter,
                        }),
                        ..Default::default()
                    },
                )
                .await?;
            }
        }
        if !carried_any || !(eff.dismiss_stale_approvals || eff.dismiss_approvals_on_new_revision) {
            return Ok(());
        }
        let same = same && !eff.dismiss_approvals_on_new_revision;
        slath_db::issues::insert_event(
            &self.pool,
            m.repo_id,
            "merge_request",
            *id.as_uuid(),
            None,
            if same {
                "approvals_carried"
            } else {
                "approvals_stale"
            },
            &serde_json::json!({"revision": seq}),
        )
        .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn review(n: u128, user: u128, seq: i32, verdict: &str) -> Review {
        Review {
            id: uuid::Uuid::from_u128(n),
            revision_seq: seq,
            reviewer_id: Some(UserId::from_uuid(uuid::Uuid::from_u128(user))),
            reviewer: Some(format!("u{user}")),
            verdict: verdict.to_owned(),
            body: String::new(),
            created_at: time::OffsetDateTime::UNIX_EPOCH,
        }
    }

    fn counted(v: &[ReviewView]) -> Vec<(u128, bool, bool)> {
        v.iter()
            .map(|r| (r.review.id.as_u128(), r.counts, r.stale))
            .collect()
    }

    #[test]
    fn approvals_carry_over_pure_rebases_only() {
        let reviews = [review(1, 7, 1, "approve")];
        // Revision 2 kept every patch, revision 3 didn't.
        let same = HashMap::from([(2, true), (3, false)]);
        assert_eq!(
            counted(&counting(&reviews, 2, &same, true)),
            vec![(1, true, false)]
        );
        assert_eq!(
            counted(&counting(&reviews, 3, &same, true)),
            vec![(1, false, true)]
        );
        // Without dismissal, approvals stay.
        assert_eq!(
            counted(&counting(&reviews, 3, &same, false)),
            vec![(1, true, false)]
        );
    }

    #[test]
    fn the_strict_rule_dismisses_on_any_new_revision() {
        let reviews = [review(1, 7, 1, "approve")];
        let same = HashMap::from([(2, true)]);
        assert_eq!(
            counted(&counting_with(&reviews, 2, &same, true, true)),
            vec![(1, false, true)]
        );
        assert_eq!(
            counted(&counting_with(&reviews, 1, &same, true, true)),
            vec![(1, true, false)]
        );
    }

    #[test]
    fn the_latest_verdict_of_each_reviewer_counts() {
        let reviews = [
            review(1, 7, 1, "approve"),
            review(2, 7, 1, "request_changes"),
            review(3, 7, 1, "comment"),
            review(4, 8, 1, "approve"),
        ];
        let got = counted(&counting(&reviews, 1, &HashMap::new(), true));
        assert_eq!(
            got,
            vec![
                (1, false, false),
                (2, true, false),
                (3, false, false),
                (4, true, false)
            ]
        );
    }
}
