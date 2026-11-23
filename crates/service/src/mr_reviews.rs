// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Reviews of merge requests: verdicts on a revision, reviewer requests
//! (users and teams) with the "review requests" inbox, which approvals
//! still count after new revisions, and the requirements a merge request
//! must meet before it can be merged (spec: verdicts and requirements).

use std::collections::HashMap;
use std::fmt::Write as _;

use slath_core::authz::{Action, Subject, UserKind};
use slath_core::ids::{MergeRequestId, UserId};
use slath_db::merge_requests::MergeRequest;
use slath_db::mr_reviews::{self as db, Review};
use slath_git::ObjectId;

use crate::Services;
use crate::error::{Result, ServiceError};
use crate::notifications::MrActivity;
use crate::protections::Effective;
use crate::repos::RepoAccess;

/// The largest raw diff read for an approval's hunk coverage (as for the
/// diff views); past it, the hunks are unknown.
const COVERAGE_DIFF_BYTES: usize = 64 * 1024 * 1024;

/// An approval's coverage as recorded: the files of the approved diff,
/// those viewed, and (all, viewed) hunks when known.
struct Snapshot {
    files_total: i32,
    seen: Vec<String>,
    hunks: Option<(i32, i32)>,
}

/// What a review says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Approve,
    RequestChanges,
    Comment,
    /// The one maintainer's review under the one-maintainer policy: never
    /// an approval, and counted only by that policy.
    AccountableReview,
}

/// Whether a verdict decides a reviewer's standing: an approval or a
/// request for changes (comments and the one maintainer's review don't).
#[must_use]
pub fn decisive(verdict: &str) -> bool {
    matches!(verdict, "approve" | "request_changes")
}

impl Verdict {
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "approve" => Some(Self::Approve),
            "request_changes" => Some(Self::RequestChanges),
            "comment" => Some(Self::Comment),
            "accountable_review" => Some(Self::AccountableReview),
            _ => None,
        }
    }

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Approve => "approve",
            Self::RequestChanges => "request_changes",
            Self::Comment => "comment",
            Self::AccountableReview => "accountable_review",
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

/// One change's verdicts that count, for the stack: who approves it and
/// who requests changes, by name.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ChangeVerdicts {
    /// The change's full display ID.
    pub display_id: String,
    pub approvers: Vec<String>,
    pub blockers: Vec<String>,
}

/// One requirement of the merge box.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Requirement {
    /// `open`, `draft`, `iterating`, `approvals`, `changes_requested`, `up_to_date`,
    /// `sensitive_paths`, `statuses` (one per required check), `threads`,
    /// `conflicts`, or `identity`.
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
    /// The target head is an ancestor of the source head (whether or not
    /// that's required); otherwise the branch can be updated.
    pub up_to_date: bool,
    /// What the counting approvers had viewed when they approved; `None`
    /// without counting approvals.
    pub coverage: Option<Coverage>,
    /// The one-maintainer policy; `None` without the protection setting.
    pub one_maintainer: Option<crate::one_maintainer::OneMaintainer>,
    /// Requirements the policy met (`approvals`, `sensitive_paths`): a
    /// merge records it.
    pub by_policy: Vec<&'static str>,
    /// Approvals nobody can give.
    pub approvals_stuck: Option<crate::one_maintainer::Stuck>,
}

/// One counting approval's coverage snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApprovalCoverage {
    pub reviewer: String,
    /// The revision approved (older than the current one when carried).
    pub revision: i32,
    pub files_viewed: usize,
    pub files_total: usize,
    /// Viewed and all hunks of the approved diff; `None` when unknown.
    pub hunks: Option<(usize, usize)>,
}

/// The coverage of the counting approvals (spec: approval coverage).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Coverage {
    pub approvals: Vec<ApprovalCoverage>,
    /// Files of the current revision's diff no counting approver viewed.
    pub unviewed: Vec<String>,
}

impl ApprovalCoverage {
    /// "r3, 12 of 15 files and 40 of 52 hunks viewed" (without the hunks
    /// when they're unknown).
    #[must_use]
    pub fn describe(&self) -> String {
        let hunks = self.hunks.map_or_else(String::new, |(viewed, total)| {
            format!(" and {viewed} of {}", plural(total, "hunk", "hunks"))
        });
        format!(
            "r{}, {} of {}{hunks} viewed",
            self.revision,
            self.files_viewed,
            plural(self.files_total, "file", "files")
        )
    }
}

pub(crate) fn met(key: &'static str, reason: impl Into<String>) -> Requirement {
    Requirement {
        key,
        state: "met",
        tone: "done",
        reason: reason.into(),
        fix: None,
    }
}

pub(crate) fn unmet(key: &'static str, reason: impl Into<String>, fix: &str) -> Requirement {
    Requirement {
        key,
        state: "unmet",
        tone: if matches!(key, "draft" | "iterating" | "approvals" | "sensitive_paths") {
            "waiting"
        } else {
            "blocking"
        },
        reason: reason.into(),
        fix: Some(fix.to_owned()),
    }
}

/// The readiness headline of a merge request that can be merged.
pub const READY: &str = "Ready to merge";

/// A merge request's readiness in short, as its readiness card heads it:
/// the tone (`done`, `waiting`, `blocking`) and the headline. The card,
/// the list, and the list's snapshot all take it from here.
#[must_use]
pub fn readiness_summary(m: &MergeRequest, mb: &Mergeability) -> (&'static str, String) {
    match m.state.as_str() {
        "merged" => return ("done", "Merged".to_owned()),
        "closed" => return ("waiting", "Closed".to_owned()),
        _ => {}
    }
    if mb.mergeable {
        return ("done", READY.to_owned());
    }
    let Some(r) = mb
        .requirements
        .iter()
        .find(|r| r.tone == "blocking")
        .or_else(|| mb.requirements.iter().find(|r| r.tone == "waiting"))
    else {
        return ("done", "Not ready".to_owned());
    };
    let headline = match r.key {
        "draft" => "Draft".to_owned(),
        "iterating" => "Iterating".to_owned(),
        "approvals" => "Approval required".to_owned(),
        "sensitive_paths" => "Maintainer approval required".to_owned(),
        "changes_requested" => "Changes requested".to_owned(),
        "statuses" if r.tone == "waiting" => "Checks running".to_owned(),
        "statuses" => "Checks failing".to_owned(),
        "conflicts" => format!("Conflicts with {}", m.target_branch),
        // One requirement for both: unresolved IDs come first, a conflict
        // recorded by jj says so in its reason.
        "identity" if r.reason.ends_with("with a conflict recorded by jj.") => {
            "Conflict recorded by jj".to_owned()
        }
        "identity" => "Commits need a change".to_owned(),
        "up_to_date" => format!("Behind {}", m.target_branch),
        "threads" => "Unresolved threads".to_owned(),
        _ => "Not ready".to_owned(),
    };
    (r.tone, headline)
}

/// The `threads` requirement from the unresolved threads that block and
/// those on changes above a prefix being landed, which don't.
fn threads_requirement(open: usize, later: usize) -> Requirement {
    let on_later = match later {
        1 => "1 is on a later change".to_owned(),
        n => format!("{n} are on later changes"),
    };
    match (open, later) {
        (0, 0) => met("threads", "No threads are unresolved."),
        (0, _) => met(
            "threads",
            format!("No unresolved threads block landing these; {on_later}."),
        ),
        _ => unmet(
            "threads",
            if later == 0 {
                format!("{} unresolved.", plural(open, "thread is", "threads are"))
            } else {
                format!(
                    "{} unresolved and {} landing these; {} more {}.",
                    plural(open, "thread is", "threads are"),
                    if open == 1 { "blocks" } else { "block" },
                    later,
                    if later == 1 {
                        "is on a later change"
                    } else {
                        "are on later changes"
                    }
                )
            },
            "Resolve them, or answer and let their authors resolve them",
        ),
    }
}

pub(crate) fn plural(n: usize, one: &str, many: &str) -> String {
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
        I::Comment(crate::issues::TimelineItem::Event(e, _))
        | I::Event(e)
        | I::Referenced(e, _) => Some((e.created_at, e.actor.clone())),
        I::Revision { revision, .. } => Some((revision.created_at, revision.pushed_by.clone())),
        I::Review(v) => Some((v.review.created_at, v.review.reviewer.clone())),
        I::Thread(t) => Some((t.thread.created_at, t.thread.author.clone())),
    }
}

/// What decides whether a verdict still counts on the current revision.
#[derive(Debug, Clone, Default)]
pub struct History {
    pub current: i32,
    /// Per revision: whether it kept every change's patch of the one
    /// before (pure rebase).
    pub same_patches: HashMap<i32, bool>,
    /// Per revision reviewed per change: its changes with their patch IDs.
    pub patches: HashMap<i32, HashMap<uuid::Uuid, Option<String>>>,
    /// Stale approvals are dismissed (otherwise verdicts stay).
    pub dismiss_stale: bool,
    /// Any new revision makes verdicts stale.
    pub strict: bool,
}

impl History {
    /// Whether a verdict on the whole of revision `seq` still counts: on
    /// the current revision, or every revision since kept every patch.
    fn carried(&self, seq: i32) -> bool {
        if self.strict {
            return seq == self.current;
        }
        !self.dismiss_stale
            || ((seq + 1)..=self.current)
                .all(|s| self.same_patches.get(&s).copied().unwrap_or(false))
    }

    /// Whether a verdict on change `k`'s version in revision `seq` still
    /// counts: the current revision has the change, and every revision
    /// since kept its patch (other changes don't matter).
    pub(crate) fn change_carried(&self, seq: i32, k: uuid::Uuid) -> bool {
        let at = |s: i32| self.patches.get(&s).and_then(|m| m.get(&k));
        if at(self.current).is_none() {
            return false;
        }
        if self.strict {
            return seq == self.current;
        }
        if !self.dismiss_stale {
            return true;
        }
        match at(seq) {
            Some(Some(p)) => {
                (seq..=self.current).all(|s| at(s).is_some_and(|q| q.as_ref() == Some(p)))
            }
            _ => false,
        }
    }

    fn counts(&self, r: &Review) -> bool {
        match r.change_id {
            None => self.carried(r.revision_seq),
            Some(k) => self.change_carried(r.revision_seq, k),
        }
    }
}

/// Which reviews count on the current revision. A review of the whole
/// revision counts when it's the reviewer's latest approval or request
/// for changes of the whole, and carried; a review of one change when
/// it's the reviewer's latest for that change (reviews of the whole
/// included), and that change kept its patch.
#[must_use]
pub fn counting(reviews: &[Review], h: &History) -> Vec<ReviewView> {
    let decisive_reviews: Vec<&Review> = reviews.iter().filter(|r| decisive(&r.verdict)).collect();
    let latest = |r: &Review| {
        decisive_reviews
            .iter()
            .rev()
            .find(|o| {
                o.reviewer_id == r.reviewer_id
                    && (o.change_id.is_none() || o.change_id == r.change_id)
                    && (r.change_id.is_some() || o.change_id.is_none())
            })
            .map(|o| o.id)
    };
    reviews
        .iter()
        .map(|r| {
            // A deferring approval replaces the reviewer's earlier verdicts
            // and never counts itself, so it's never stale either.
            let decisive = decisive(&r.verdict)
                && !r.deferred
                && r.reviewer_id.is_some()
                && latest(r) == Some(r.id);
            let current = decisive && h.counts(r);
            ReviewView {
                review: r.clone(),
                counts: current,
                stale: decisive && !current,
            }
        })
        .collect()
}

/// Who approves and who requests changes for one change of the current
/// revision (`None`: the revision, reviewed as a whole).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Standing {
    pub change: Option<uuid::Uuid>,
    /// (reviewer, review) of each counting approval.
    pub approvals: Vec<(UserId, uuid::Uuid)>,
    pub blockers: Vec<UserId>,
}

/// Each change's standing, in `order` (the current revision's changes,
/// bottom first; empty when it's reviewed as a whole). For each change,
/// a reviewer's latest verdict on the whole or on that change decides.
#[must_use]
pub fn standings(reviews: &[Review], h: &History, order: &[uuid::Uuid]) -> Vec<Standing> {
    let keys: Vec<Option<uuid::Uuid>> = if order.is_empty() {
        vec![None]
    } else {
        order.iter().copied().map(Some).collect()
    };
    keys.into_iter()
        .map(|k| {
            let mut latest: Vec<(UserId, &Review)> = Vec::new();
            for r in reviews.iter().filter(|r| {
                decisive(&r.verdict) && (r.change_id.is_none() || (k.is_some() && r.change_id == k))
            }) {
                let Some(u) = r.reviewer_id else { continue };
                match latest.iter_mut().find(|(v, _)| *v == u) {
                    Some(e) => e.1 = r,
                    None => latest.push((u, r)),
                }
            }
            let mut out = Standing {
                change: k,
                approvals: Vec::new(),
                blockers: Vec::new(),
            };
            for (u, r) in latest
                .into_iter()
                .filter(|(_, r)| !r.deferred && h.counts(r))
            {
                if r.verdict == "approve" {
                    out.approvals.push((u, r.id));
                } else {
                    out.blockers.push(u);
                }
            }
            out
        })
        .collect()
}

impl Services {
    pub(crate) async fn effective_for(&self, m: &MergeRequest) -> Result<Effective> {
        let rules = slath_db::protections::list(&self.pool, m.repo_id).await?;
        Ok(crate::protections::effective(
            &rules,
            m.target_branch.as_bytes(),
        ))
    }

    /// The status contexts the target branch requires.
    pub async fn mr_required_contexts(&self, m: &MergeRequest) -> Result<Vec<String>> {
        Ok(self
            .effective_for(m)
            .await?
            .required_checks
            .into_iter()
            .map(|c| c.context)
            .collect())
    }

    /// Submits a review of the current revision, or with `change` (its
    /// display ID, or at least its first 8 characters) of that change's
    /// version in it. The author may only comment; a comment needs a body.
    /// An approval with `defer_to` (a user name or `org/team`) leaves the
    /// decision to them: it never counts, and asks them for a review.
    pub async fn submit_review(
        &self,
        subject: &Subject,
        (ns, repo, number): (&str, &str, i32),
        verdict: &str,
        body: &str,
        (change, defer_to): (Option<&str>, Option<&str>),
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
        if verdict == Verdict::AccountableReview {
            self.check_one_maintainer(&a, &m, *me, change).await?;
        } else if verdict != Verdict::Comment && m.author_id == Some(*me) {
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
        let change = match change.map(str::trim).filter(|c| !c.is_empty()) {
            Some(c) => Some(self.review_target(&m, c).await?),
            None => None,
        };
        let defer = match defer_to.map(str::trim).filter(|d| !d.is_empty()) {
            Some(_) if verdict != Verdict::Approve => {
                return Err(ServiceError::invalid(
                    "defer_to",
                    "verdict",
                    "Only an approval can leave the decision to someone else.",
                ));
            }
            Some(d) => Some(self.defer_target(&a, &m, *me, d).await?),
            None => None,
        };
        // An approval's coverage: the files of the revision's (or the
        // change's) diff the approver viewed, as they are in it. A
        // deferring approval counts for nothing, so it takes none.
        let coverage = if (verdict == Verdict::Approve && defer.is_none())
            || verdict == Verdict::AccountableReview
        {
            Some(self.coverage_snapshot(&m, *me, change.as_ref()).await?)
        } else {
            None
        };
        let origin = self.origin_of(subject, &a.repo).await?;
        // Pending replies may resolve their threads: those the reviewer
        // may still resolve now, and that are still open.
        let threads = self.mr_threads(subject, &a, &m).await?;
        let mut tx = self.pool.begin().await.map_err(ServiceError::internal)?;
        let id = db::insert(
            &mut *tx,
            m.id,
            (m.revisions, change.as_ref().map(|v| v.0)),
            *me,
            (verdict.as_str(), defer.as_ref().map(|d| d.0)),
            body.trim(),
            origin,
        )
        .await?;
        if let Some(c) = coverage {
            db::set_coverage(&mut tx, id, (c.files_total, &c.seen), c.hunks).await?;
        }
        // The review publishes the reviewer's pending comments with it,
        // and resolves the threads they say to.
        let published = slath_db::mr_threads::publish_pending(&mut *tx, m.id, *me).await?;
        for t in threads
            .iter()
            .filter(|t| published.resolve.contains(&t.thread.id))
        {
            if t.thread.resolved_at.is_none() && Self::may_resolve(subject, &a, &m, t).is_ok() {
                Self::resolve_in(&mut tx, &a, &m, t, *me).await?;
            }
        }
        db::mark_reviewed(&mut *tx, m.id, *me).await?;
        // After the reviewer's own requests are done, so a team they're
        // in stays asked.
        let asked = match &defer {
            Some((to, name)) => {
                self.request_deferred(&mut tx, (&a, &m), *me, (*to, name))
                    .await?
            }
            None => Vec::new(),
        };
        slath_db::merge_requests::touch(&mut *tx, m.id).await?;
        tx.commit().await.map_err(ServiceError::internal)?;
        let review = db::list(&self.pool, m.id)
            .await?
            .pop()
            .ok_or(ServiceError::NotFound)?;
        self.notify_mr(Some(*me), &m, MrActivity::Reviewed(review.id))
            .await;
        if !asked.is_empty() {
            self.notify_mr(Some(*me), &m, MrActivity::ReviewRequested(asked))
                .await;
        }
        self.queue_auto_merge(m.id).await;
        Ok(review)
    }

    /// The files of revision `seq`'s diff, with their objects at its head.
    async fn revision_files(&self, m: &MergeRequest, seq: i32) -> Result<Vec<(String, String)>> {
        let revs = slath_db::merge_requests::revisions(&self.pool, m.id).await?;
        let r = revs
            .iter()
            .find(|r| r.seq == seq)
            .ok_or(ServiceError::NotFound)?;
        let parse = |s: &str| slath_git::ObjectId::parse(s).map_err(ServiceError::internal);
        let base = r.merge_base_oid.as_deref().map(parse).transpose()?;
        let head = parse(&r.head_oid)?;
        let dir = self.repo_path(m.repo_id);
        Ok(
            slath_git::browse::changed_blobs(&self.runner, &dir, base.as_ref(), &head)
                .await
                .map_err(ServiceError::internal)?
                .into_iter()
                .map(|(p, o)| (String::from_utf8_lossy(&p).into_owned(), o))
                .collect(),
        )
    }

    /// The change a review of one change targets, with its version's
    /// parent and commit in the current revision. Only revisions reviewed
    /// per change have changes, and only bound commits can be reviewed.
    async fn review_target(
        &self,
        m: &MergeRequest,
        id: &str,
    ) -> Result<(uuid::Uuid, Option<ObjectId>, ObjectId)> {
        let versions = slath_db::mr_changes::versions(&self.pool, m.id).await?;
        let here: Vec<_> = versions
            .iter()
            .filter(|v| v.revision_seq == m.revisions)
            .collect();
        if here.is_empty() {
            return Err(ServiceError::invalid(
                "change",
                "whole",
                "This revision is reviewed as a whole; review it without choosing a change.",
            ));
        }
        let changes = slath_db::mr_changes::changes(&self.pool, m.id).await?;
        let named: Vec<_> = changes
            .iter()
            .filter(|c| c.display_id == id || (id.len() >= 8 && c.display_id.starts_with(id)))
            .filter_map(|c| here.iter().find(|v| v.change_id == Some(c.id)))
            .collect();
        let [v] = named.as_slice() else {
            return Err(ServiceError::invalid(
                "change",
                "unknown",
                "The current revision has no such change.",
            ));
        };
        let parse = |s: &str| ObjectId::parse(s).map_err(ServiceError::internal);
        Ok((
            v.change_id
                .ok_or_else(|| ServiceError::internal("unbound version"))?,
            v.parent_oid.as_deref().map(parse).transpose()?,
            parse(&v.commit_oid)?,
        ))
    }

    /// The coverage of `user`'s approval of the current revision, or of
    /// one change's version: how many files its diff has, which the user
    /// viewed as they are in it (marked at that blob, or every hunk
    /// viewed), and its hunks with the viewed ones (`None` past the diff
    /// limits).
    async fn coverage_snapshot(
        &self,
        m: &MergeRequest,
        user: UserId,
        change: Option<&(uuid::Uuid, Option<ObjectId>, ObjectId)>,
    ) -> Result<Snapshot> {
        let parse = |s: &str| ObjectId::parse(s).map_err(ServiceError::internal);
        let (base, head) = match change {
            None => {
                let revs = slath_db::merge_requests::revisions(&self.pool, m.id).await?;
                let r = revs
                    .iter()
                    .find(|r| r.seq == m.revisions)
                    .ok_or(ServiceError::NotFound)?;
                (
                    r.merge_base_oid.as_deref().map(parse).transpose()?,
                    parse(&r.head_oid)?,
                )
            }
            Some((_, parent, commit)) => (parent.clone(), commit.clone()),
        };
        let dir = self.repo_path(m.repo_id);
        let files: Vec<(String, String)> =
            slath_git::browse::changed_blobs(&self.runner, &dir, base.as_ref(), &head)
                .await
                .map_err(ServiceError::internal)?
                .into_iter()
                .map(|(p, o)| (String::from_utf8_lossy(&p).into_owned(), o))
                .collect();
        let viewed: HashMap<String, String> = slath_db::mr_threads::viewed(&self.pool, m.id, user)
            .await?
            .into_iter()
            .collect();
        let marked: std::collections::HashSet<(String, String)> =
            slath_db::mr_threads::viewed_hunks(&self.pool, m.id, user)
                .await?
                .into_iter()
                .collect();
        let (raw, cut) = slath_git::browse::diff(
            &self.runner,
            &dir,
            base.as_ref(),
            &head,
            slath_git::browse::DiffOptions::default(),
            COVERAGE_DIFF_BYTES,
        )
        .await
        .map_err(ServiceError::internal)?;
        let limits = &self.config.limits;
        let diff = slath_git::diff::parse(&raw, limits.diff_max_files, limits.diff_max_lines);
        // Each file's hunks: (all, viewed).
        let mut by_file: HashMap<String, (usize, usize)> = HashMap::new();
        for f in &diff.files {
            let path = String::from_utf8_lossy(f.path()).into_owned();
            let file_mark = files
                .iter()
                .any(|(p, o)| *p == path && viewed.get(p) == Some(o));
            let keys = slath_git::diff::hunk_keys(f);
            let seen = keys
                .iter()
                .filter(|k| file_mark || marked.contains(&(path.clone(), (*k).clone())))
                .count();
            by_file.insert(path, (keys.len(), seen));
        }
        let seen = files
            .iter()
            .filter(|(p, o)| {
                viewed.get(p) == Some(o)
                    || by_file
                        .get(p)
                        .is_some_and(|(all, seen)| *all > 0 && all == seen)
            })
            .map(|(p, _)| p.clone())
            .collect();
        let hunks = (!cut && !diff.truncated).then(|| {
            let (all, seen) = by_file
                .values()
                .fold((0, 0), |(a, s), (fa, fs)| (a + fa, s + fs));
            (
                i32::try_from(all).unwrap_or(i32::MAX),
                i32::try_from(seen).unwrap_or(i32::MAX),
            )
        });
        Ok(Snapshot {
            files_total: i32::try_from(files.len()).unwrap_or(i32::MAX),
            seen,
            hunks,
        })
    }

    /// The coverage of the counting approvals `approvals` (review, name).
    pub(crate) async fn approval_coverage(
        &self,
        m: &MergeRequest,
        approvals: &[(&Review, String)],
    ) -> Result<Option<Coverage>> {
        if approvals.is_empty() {
            return Ok(None);
        }
        let ids: Vec<uuid::Uuid> = approvals.iter().map(|(r, _)| r.id).collect();
        let snaps = db::coverage(&self.pool, &ids).await?;
        let mut seen = std::collections::HashSet::new();
        let mut out = Coverage::default();
        for (r, name) in approvals {
            let Some(c) = snaps.get(&r.id) else {
                continue;
            };
            seen.extend(c.viewed.iter().cloned());
            out.approvals.push(ApprovalCoverage {
                reviewer: name.clone(),
                revision: r.revision_seq,
                files_viewed: c.viewed.len(),
                files_total: usize::try_from(c.files_total).unwrap_or(0),
                hunks: c.hunks.map(|(total, viewed)| {
                    (
                        usize::try_from(viewed).unwrap_or(0),
                        usize::try_from(total).unwrap_or(0),
                    )
                }),
            });
        }
        out.unviewed = self
            .revision_files(m, m.revisions)
            .await?
            .into_iter()
            .map(|(p, _)| p)
            .filter(|p| !seen.contains(p))
            .collect();
        Ok(Some(out))
    }

    /// What decides whether verdicts still count on revision `current`.
    pub(crate) async fn history(&self, m: &MergeRequest, current: i32) -> Result<History> {
        let same_patches = db::same_patches(&self.pool, m.id)
            .await?
            .into_iter()
            .filter(|(s, _)| *s <= current)
            .map(|(s, v)| (s, v.unwrap_or(false)))
            .collect();
        let mut patches: HashMap<i32, HashMap<uuid::Uuid, Option<String>>> = HashMap::new();
        for v in slath_db::mr_changes::versions(&self.pool, m.id).await? {
            if let Some(k) = v.change_id {
                patches
                    .entry(v.revision_seq)
                    .or_default()
                    .insert(k, v.patch_id);
            }
        }
        let eff = self.effective_for(m).await?;
        Ok(History {
            current,
            same_patches,
            patches,
            dismiss_stale: eff.dismiss_stale_approvals,
            strict: eff.dismiss_approvals_on_new_revision,
        })
    }

    /// The reviews, with whether each still counts.
    pub async fn mr_reviews(&self, m: &MergeRequest) -> Result<Vec<ReviewView>> {
        let reviews = db::list(&self.pool, m.id).await?;
        Ok(counting(&reviews, &self.history(m, m.revisions).await?))
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
    /// The team `org/team` of the repository's organization.
    async fn repo_team(&self, a: &RepoAccess, field: &str, name: &str) -> Result<uuid::Uuid> {
        let found = match name.split_once('/') {
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
        found.map(|t| t.id).ok_or_else(|| {
            ServiceError::invalid(
                field,
                "unknown",
                format!(
                    "{name} isn't a team of the organization {}.",
                    a.repo.namespace
                ),
            )
        })
    }

    /// Whom a deferring approval by `me` leaves the decision to: a reader
    /// other than `me` and the author, or a team of the repository's
    /// organization.
    async fn defer_target(
        &self,
        a: &RepoAccess,
        m: &MergeRequest,
        me: UserId,
        name: &str,
    ) -> Result<(db::DeferTo, String)> {
        let name = crate::one_name(name);
        if name.contains('/') {
            let t = self.repo_team(a, "defer_to", name).await?;
            return Ok((db::DeferTo::Team(t), name.to_owned()));
        }
        let users = self.readers(a, "defer_to", &[name.to_owned()]).await?;
        match users.first() {
            Some(u) if *u == me => Err(ServiceError::invalid(
                "defer_to",
                "self",
                "Name someone else to leave the decision to.",
            )),
            Some(u) if m.author_id == Some(*u) => Err(ServiceError::invalid(
                "defer_to",
                "author",
                "The author can't decide on their own merge request.",
            )),
            Some(u) => Ok((db::DeferTo::User(*u), name.to_owned())),
            None => Err(ServiceError::invalid(
                "defer_to",
                "required",
                "Name a user or a team.",
            )),
        }
    }

    /// Asks the user or team a deferring approval names for a review,
    /// unless they're already asked; returns the users to notify.
    async fn request_deferred(
        &self,
        tx: &mut sqlx::PgConnection,
        (a, m): (&RepoAccess, &MergeRequest),
        me: UserId,
        (to, name): (db::DeferTo, &str),
    ) -> Result<Vec<UserId>> {
        let before = db::requested(&mut *tx, m.id).await?;
        let asked = before.iter().any(|b| {
            b.state == "requested"
                && match to {
                    db::DeferTo::User(u) => b.user == Some(u),
                    db::DeferTo::Team(t) => b.team == Some(t),
                }
        });
        if asked {
            return Ok(Vec::new());
        }
        let notify = match to {
            db::DeferTo::User(u) => {
                slath_db::merge_requests::add_reviewer(&mut *tx, m.id, u, me).await?;
                vec![u]
            }
            db::DeferTo::Team(t) => {
                db::request_team(&mut *tx, m.id, t, me).await?;
                slath_db::teams::members(&mut *tx, t)
                    .await?
                    .into_iter()
                    .map(|(u, _)| u)
                    .filter(|u| m.author_id != Some(*u) && *u != me)
                    .collect()
            }
        };
        slath_db::issues::insert_event(
            &mut *tx,
            a.repo.id,
            "merge_request",
            *m.id.as_uuid(),
            Some(me),
            "review_requested",
            &serde_json::json!({"user": name}),
        )
        .await?;
        Ok(notify)
    }

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
            teams.push((self.repo_team(&a, "reviewers", t).await?, t.clone()));
        }
        let before = db::requested(&self.pool, m.id).await?;
        let mut tx = self.pool.begin().await.map_err(ServiceError::internal)?;
        let event = |kind: &'static str, who: String| (kind, serde_json::json!({"user": who}));
        let mut events = Vec::new();
        let mut asked = Vec::new();
        for u in &users {
            if !before.iter().any(|b| b.user == Some(*u)) {
                asked.push(*u);
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
                asked.extend(
                    slath_db::teams::members(&self.pool, *t)
                        .await?
                        .into_iter()
                        .map(|(u, _)| u)
                        .filter(|u| m.author_id != Some(*u)),
                );
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
        if !asked.is_empty() {
            self.notify_mr(Some(*me), &m, MrActivity::ReviewRequested(asked))
                .await;
        }
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
    pub(crate) async fn verdict_counts(
        &self,
        a: &RepoAccess,
        user: UserId,
        bots: bool,
    ) -> Result<bool> {
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

    /// The paths the current revision's whole diff changes (none before
    /// the first revision), up to `m.head_oid`.
    pub async fn mr_changed_paths(&self, m: &MergeRequest) -> Result<Vec<Vec<u8>>> {
        let Some(rev) = slath_db::merge_requests::revisions(&self.pool, m.id)
            .await?
            .pop()
        else {
            return Ok(Vec::new());
        };
        let parse = |s: &str| ObjectId::parse(s).map_err(ServiceError::internal);
        let base = rev.merge_base_oid.as_deref().map(parse).transpose()?;
        // A prefix that lands has a head of its own.
        let head = parse(m.head_oid.as_deref().unwrap_or(&rev.head_oid))?;
        slath_git::browse::changed_paths(
            &self.runner,
            &self.repo_path(m.repo_id),
            base.as_ref(),
            &head,
        )
        .await
        .map_err(ServiceError::internal)
    }

    /// The sensitive paths the current revision's whole diff changes.
    pub async fn mr_sensitive_changes(&self, m: &MergeRequest) -> Result<Vec<String>> {
        let sensitive = self.sensitive_paths(m.repo_id).await?;
        Ok(self
            .mr_changed_paths(m)
            .await?
            .into_iter()
            .filter(|p| sensitive.matches(p))
            .map(|p| String::from_utf8_lossy(&p).into_owned())
            .collect())
    }

    /// The `sensitive_paths` requirement: on a protected target, changes
    /// to sensitive paths need an approval of every change by a person
    /// with the maintain role or higher, other than the author
    /// (`maintainers`). Bots never satisfy it.
    /// The sensitive-path requirement, and whether the one-maintainer
    /// policy met it.
    async fn sensitive_requirement(
        &self,
        m: &MergeRequest,
        maintainers: &[String],
        policy: Option<&crate::one_maintainer::PolicyReview>,
    ) -> Result<Option<(Requirement, bool)>> {
        let touched = self.mr_sensitive_changes(m).await?;
        if touched.is_empty() {
            return Ok(None);
        }
        let mut list = touched
            .iter()
            .take(10)
            .map(String::as_str)
            .collect::<Vec<_>>()
            .join(", ");
        if touched.len() > 10 {
            let _ = write!(list, " and {} more", touched.len() - 10);
        }
        Ok(Some(match (maintainers.is_empty(), policy) {
            (true, Some(p)) => p.sensitive(&list),
            (true, None) => (
                unmet(
                    "sensitive_paths",
                    format!("Changes sensitive paths: {list}. Needs an approval by a maintainer."),
                    "Ask a maintainer to review",
                ),
                false,
            ),
            (false, _) => (
                met(
                    "sensitive_paths",
                    format!(
                        "Changes sensitive paths ({list}); approved by a maintainer: {}.",
                        maintainers.join(", ")
                    ),
                ),
                false,
            ),
        }))
    }

    /// The requirements of the merge box for the current revision.
    pub async fn mr_mergeability(&self, a: &RepoAccess, m: &MergeRequest) -> Result<Mergeability> {
        self.mr_mergeability_upto(a, m, None).await
    }

    /// The requirements for landing `prefix` (the bottom changes of the
    /// stack), or the whole merge request (`None`). Approvals, requests
    /// for changes, and sensitive paths count the prefix's changes; checks,
    /// conflicts, and being up to date are those of its top commit; drafts
    /// and unresolved threads are the merge request's.
    pub async fn mr_mergeability_upto(
        &self,
        a: &RepoAccess,
        m: &MergeRequest,
        prefix: Option<&crate::partial_landing::Prefix>,
    ) -> Result<Mergeability> {
        let upto = prefix.map(|p| {
            let mut x = m.clone();
            x.head_oid = Some(p.head.clone());
            x
        });
        // What lands: the prefix's top commit, or the head.
        let lands = upto.as_ref().unwrap_or(m);
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
        if m.attention == "iterating" {
            reqs.push(unmet(
                "iterating",
                "The bot is still iterating.",
                "Mark it as ready",
            ));
        }
        let one_maintainer = self.one_maintainer(&a.repo, &eff).await?;
        let review = self
            .review_requirements(a, m, &eff, prefix.map(|p| p.len))
            .await?;
        let coverage = review.coverage;
        let approvals_stuck = review.stuck;
        let mut by_policy = Vec::new();
        if review.by_policy {
            by_policy.push("approvals");
        }
        reqs.push(review.approvals);
        reqs.push(review.changes_requested);
        let (up_to_date, up_req) = self.up_to_date(lands).await?;
        if eff.require_up_to_date {
            reqs.push(up_req);
        }
        if eff.protected
            && let Some((r, policy)) = self
                .sensitive_requirement(lands, &review.maintainers, review.policy.as_ref())
                .await?
        {
            if policy {
                by_policy.push("sensitive_paths");
            }
            reqs.push(r);
        }
        reqs.extend(self.check_requirements(lands, &eff, up_to_date).await?);
        if eff.require_resolved_threads {
            let (open, later) = self
                .unresolved_threads_upto(m, prefix.map(|p| p.changes.as_slice()))
                .await?;
            reqs.push(threads_requirement(open, later));
        }
        if m.state == "open" {
            let files = self.mr_conflicts(lands).await?;
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
        let landing = &stack.entries[..prefix.map_or(stack.entries.len(), |p| p.len)];
        let unresolved = landing.iter().filter(|e| e.binding == "unresolved").count();
        let conflicted = landing.iter().filter(|e| e.conflicted).count();
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
                "Resolve it and push again; the page of each change marked Conflict on the Changes tab shows how",
            )
        } else {
            met("identity", "Every commit belongs to one change.")
        });
        Ok(Mergeability {
            mergeable: reqs.iter().all(|r| r.state == "met"),
            requirements: reqs,
            up_to_date,
            coverage,
            one_maintainer,
            by_policy,
            approvals_stuck,
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
        let before = counting(&reviews, &self.history(&m, seq - 1).await?);
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
            .filter(|v| v.counts && v.review.verdict == "approve" && v.review.change_id.is_none())
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
        // Approvals of one change: each carries or goes stale on its own.
        let after = self.history(&m, seq).await?;
        let mut stale_changes: Vec<String> = Vec::new();
        for v in before
            .iter()
            .filter(|v| v.counts && v.review.verdict == "approve")
        {
            let Some(k) = v.review.change_id else {
                continue;
            };
            let has = after.patches.get(&seq).is_some_and(|p| p.contains_key(&k));
            let (kind, reason) = if after.change_carried(v.review.revision_seq, k) {
                let why = if eff.dismiss_stale_approvals {
                    "same-patch"
                } else {
                    "not-dismissed"
                };
                ("approval_carried", why)
            } else if here.is_empty() {
                ("approval_stale", "whole-only")
            } else if !has {
                ("approval_stale", "removed")
            } else if eff.dismiss_approvals_on_new_revision {
                ("approval_stale", "new-revision")
            } else {
                ("approval_stale", "patch-changed")
            };
            if kind == "approval_stale"
                && let Some(c) = &v.review.change
            {
                let short: String = c.chars().take(8).collect();
                if !stale_changes.contains(&short) {
                    stale_changes.push(short);
                }
            }
            slath_db::mr_ledger::append(
                &self.pool,
                id,
                &slath_db::mr_ledger::NewEntry {
                    revision_seq: Some(seq),
                    kind,
                    change_id: v.review.change.as_deref(),
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
        if !stale_changes.is_empty() {
            slath_db::issues::insert_event(
                &self.pool,
                m.repo_id,
                "merge_request",
                *id.as_uuid(),
                None,
                "approvals_stale",
                &serde_json::json!({"revision": seq, "changes": stale_changes}),
            )
            .await?;
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

    #[test]
    fn threads_on_later_changes_are_named_but_dont_block() {
        let r = super::threads_requirement(0, 0);
        assert_eq!(
            (r.state, r.reason.as_str()),
            ("met", "No threads are unresolved.")
        );
        let r = super::threads_requirement(0, 2);
        assert_eq!(
            (r.state, r.reason.as_str()),
            (
                "met",
                "No unresolved threads block landing these; 2 are on later changes."
            )
        );
        let r = super::threads_requirement(1, 1);
        assert_eq!(
            (r.state, r.reason.as_str()),
            (
                "unmet",
                "1 thread is unresolved and blocks landing these; 1 more is on a later change."
            )
        );
        let r = super::threads_requirement(3, 0);
        assert_eq!(
            (r.state, r.reason.as_str()),
            ("unmet", "3 threads are unresolved.")
        );
    }
    use super::*;

    fn review(n: u128, user: u128, seq: i32, verdict: &str) -> Review {
        Review {
            id: uuid::Uuid::from_u128(n),
            revision_seq: seq,
            reviewer_id: Some(UserId::from_uuid(uuid::Uuid::from_u128(user))),
            reviewer: Some(format!("u{user}")),
            verdict: verdict.to_owned(),
            body: String::new(),
            origin: slath_core::origin::Origin::Writer,
            created_at: time::OffsetDateTime::UNIX_EPOCH,
            change_id: None,
            change: None,
            deferred: false,
            defer_to: None,
        }
    }

    fn deferring(n: u128, user: u128, seq: i32) -> Review {
        Review {
            deferred: true,
            defer_to: Some("bob".to_owned()),
            ..review(n, user, seq, "approve")
        }
    }

    #[test]
    fn conf_review_19_a_deferring_approval_never_counts() {
        let h = history(1, &[], true, false);
        let alone = [deferring(1, 7, 1)];
        assert_eq!(counted(&counting(&alone, &h)), vec![(1, false, false)]);
        assert_eq!(
            standings(&alone, &h, &[]),
            vec![Standing {
                change: None,
                approvals: vec![],
                blockers: vec![],
            }]
        );
        // It replaces the reviewer's earlier approval and request for
        // changes, of the whole and of a change.
        let k = uuid::Uuid::from_u128(100);
        let later = [
            review(1, 7, 1, "approve"),
            on(2, 7, 1, "request_changes", 100),
            deferring(3, 7, 1),
        ];
        assert_eq!(
            counted(&counting(&later, &h)),
            vec![(1, false, false), (2, false, false), (3, false, false)]
        );
        let h = History {
            patches: HashMap::from([(1, HashMap::from([(k, Some("p".to_owned()))]))]),
            ..h
        };
        let got = standings(&later, &h, &[k]);
        assert!(got[0].approvals.is_empty() && got[0].blockers.is_empty());
        // A later ordinary approval counts again.
        let again = [deferring(1, 7, 1), review(2, 7, 1, "approve")];
        assert_eq!(
            counted(&counting(&again, &h)),
            vec![(1, false, false), (2, true, false)]
        );
    }

    fn history(current: i32, same: &[(i32, bool)], dismiss_stale: bool, strict: bool) -> History {
        History {
            current,
            same_patches: same.iter().copied().collect(),
            dismiss_stale,
            strict,
            ..History::default()
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
        let same = [(2, true), (3, false)];
        assert_eq!(
            counted(&counting(&reviews, &history(2, &same, true, false))),
            vec![(1, true, false)]
        );
        assert_eq!(
            counted(&counting(&reviews, &history(3, &same, true, false))),
            vec![(1, false, true)]
        );
        // Without dismissal, approvals stay.
        assert_eq!(
            counted(&counting(&reviews, &history(3, &same, false, false))),
            vec![(1, true, false)]
        );
    }

    #[test]
    fn the_strict_rule_dismisses_on_any_new_revision() {
        let reviews = [review(1, 7, 1, "approve")];
        let same = [(2, true)];
        assert_eq!(
            counted(&counting(&reviews, &history(2, &same, true, true))),
            vec![(1, false, true)]
        );
        assert_eq!(
            counted(&counting(&reviews, &history(1, &same, true, true))),
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
        let got = counted(&counting(&reviews, &history(1, &[], true, false)));
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

    fn on(n: u128, user: u128, seq: i32, verdict: &str, change: u128) -> Review {
        Review {
            change_id: Some(uuid::Uuid::from_u128(change)),
            ..review(n, user, seq, verdict)
        }
    }

    /// Revisions 1..=3 of changes 100 and 200: 100 keeps its patch, 200's
    /// changes in revision 3.
    fn stack_history() -> History {
        let k = |c: u128| uuid::Uuid::from_u128(c);
        let rev = |a: &str, b: &str| {
            HashMap::from([(k(100), Some(a.to_owned())), (k(200), Some(b.to_owned()))])
        };
        History {
            current: 3,
            same_patches: HashMap::from([(2, true), (3, false)]),
            patches: HashMap::from([(1, rev("a", "b")), (2, rev("a", "b")), (3, rev("a", "b2"))]),
            dismiss_stale: true,
            strict: false,
        }
    }

    #[test]
    fn change_verdicts_go_stale_with_their_own_patch_only() {
        let h = stack_history();
        let reviews = [on(1, 7, 1, "approve", 100), on(2, 8, 1, "approve", 200)];
        assert_eq!(
            counted(&counting(&reviews, &h)),
            vec![(1, true, false), (2, false, true)]
        );
        // A change the current revision doesn't have counts for nothing.
        let gone = [on(3, 7, 3, "approve", 300)];
        assert_eq!(counted(&counting(&gone, &h)), vec![(3, false, true)]);
        // Under the strict rule only verdicts on the current revision count.
        let strict = History {
            strict: true,
            ..stack_history()
        };
        let reviews = [on(1, 7, 2, "approve", 100), on(2, 7, 3, "approve", 200)];
        assert_eq!(
            counted(&counting(&reviews, &strict)),
            vec![(1, false, true), (2, true, false)]
        );
    }

    #[test]
    fn a_later_verdict_on_the_whole_decides_for_each_change() {
        let h = stack_history();
        // bob approves change 100, then requests changes on the whole.
        let reviews = [
            on(1, 7, 3, "approve", 100),
            review(2, 7, 3, "request_changes"),
        ];
        assert_eq!(
            counted(&counting(&reviews, &h)),
            vec![(1, false, false), (2, true, false)]
        );
        // A later verdict on one change doesn't hide one on the whole.
        let reviews = [
            review(1, 7, 3, "approve"),
            on(2, 7, 3, "request_changes", 100),
        ];
        assert_eq!(
            counted(&counting(&reviews, &h)),
            vec![(1, true, false), (2, true, false)]
        );
    }
}
