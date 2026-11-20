// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Reviews of merge requests: verdicts on a revision, reviewer requests
//! (users and teams) with the "review requests" inbox, which approvals
//! still count after new revisions, and the requirements a merge request
//! must meet before it can be merged (spec: verdicts and requirements).

use std::collections::HashMap;
use std::fmt::Write as _;

use slath_core::authz::{Action, Role, Subject, UserKind};
use slath_core::ids::{MergeRequestId, UserId};
use slath_db::merge_requests::MergeRequest;
use slath_db::mr_reviews::{self as db, Review};
use slath_git::ObjectId;

use crate::Services;
use crate::error::{Result, ServiceError};
use crate::notifications::MrActivity;
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
}

/// One counting approval's coverage snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApprovalCoverage {
    pub reviewer: String,
    /// The revision approved (older than the current one when carried).
    pub revision: i32,
    pub files_viewed: usize,
    pub files_total: usize,
}

/// The coverage of the counting approvals (spec: approval coverage).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Coverage {
    pub approvals: Vec<ApprovalCoverage>,
    /// Files of the current revision's diff no counting approver viewed.
    pub unviewed: Vec<String>,
}

impl ApprovalCoverage {
    /// "r3, 12 of 15 files viewed".
    #[must_use]
    pub fn describe(&self) -> String {
        format!(
            "r{}, {} of {} viewed",
            self.revision,
            self.files_viewed,
            plural(self.files_total, "file", "files")
        )
    }
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
        tone: if matches!(key, "draft" | "iterating" | "approvals" | "sensitive_paths") {
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
    fn change_carried(&self, seq: i32, k: uuid::Uuid) -> bool {
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
    let decisive: Vec<&Review> = reviews.iter().filter(|r| r.verdict != "comment").collect();
    let latest = |r: &Review| {
        decisive
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
            let decisive =
                r.verdict != "comment" && r.reviewer_id.is_some() && latest(r) == Some(r.id);
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
                r.verdict != "comment"
                    && (r.change_id.is_none() || (k.is_some() && r.change_id == k))
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
            for (u, r) in latest.into_iter().filter(|(_, r)| h.counts(r)) {
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
    pub async fn submit_review(
        &self,
        subject: &Subject,
        (ns, repo, number): (&str, &str, i32),
        verdict: &str,
        body: &str,
        change: Option<&str>,
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
        let change = match change.map(str::trim).filter(|c| !c.is_empty()) {
            Some(c) => Some(self.review_target(&m, c).await?),
            None => None,
        };
        // An approval's coverage: the files of the revision's (or the
        // change's) diff the approver viewed, as they are in it.
        let coverage = if verdict == Verdict::Approve {
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
            verdict.as_str(),
            body.trim(),
            origin,
        )
        .await?;
        if let Some((total, viewed)) = coverage {
            db::set_coverage(&mut tx, id, total, &viewed).await?;
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
        slath_db::merge_requests::touch(&mut *tx, m.id).await?;
        tx.commit().await.map_err(ServiceError::internal)?;
        let review = db::list(&self.pool, m.id)
            .await?
            .pop()
            .ok_or(ServiceError::NotFound)?;
        self.notify_mr(Some(*me), &m, MrActivity::Reviewed(review.id))
            .await;
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
    /// one change's version: how many files its diff has, and which the
    /// user viewed as they are in it.
    async fn coverage_snapshot(
        &self,
        m: &MergeRequest,
        user: UserId,
        change: Option<&(uuid::Uuid, Option<ObjectId>, ObjectId)>,
    ) -> Result<(i32, Vec<String>)> {
        let files = match change {
            None => self.revision_files(m, m.revisions).await?,
            Some((_, parent, commit)) => slath_git::browse::changed_blobs(
                &self.runner,
                &self.repo_path(m.repo_id),
                parent.as_ref(),
                commit,
            )
            .await
            .map_err(ServiceError::internal)?
            .into_iter()
            .map(|(p, o)| (String::from_utf8_lossy(&p).into_owned(), o))
            .collect(),
        };
        let viewed: HashMap<String, String> = slath_db::mr_threads::viewed(&self.pool, m.id, user)
            .await?
            .into_iter()
            .collect();
        let seen = files
            .iter()
            .filter(|(p, o)| viewed.get(p) == Some(o))
            .map(|(p, _)| p.clone())
            .collect();
        Ok((i32::try_from(files.len()).unwrap_or(i32::MAX), seen))
    }

    /// The coverage of the counting approvals `approvals` (review, name).
    async fn approval_coverage(
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

    /// The sensitive paths the current revision's whole diff changes.
    pub async fn mr_sensitive_changes(&self, m: &MergeRequest) -> Result<Vec<String>> {
        let Some(rev) = slath_db::merge_requests::revisions(&self.pool, m.id)
            .await?
            .pop()
        else {
            return Ok(Vec::new());
        };
        let parse = |s: &str| ObjectId::parse(s).map_err(ServiceError::internal);
        let base = rev.merge_base_oid.as_deref().map(parse).transpose()?;
        let head = parse(&rev.head_oid)?;
        let sensitive = self.sensitive_paths(m.repo_id).await?;
        let paths = slath_git::browse::changed_paths(
            &self.runner,
            &self.repo_path(m.repo_id),
            base.as_ref(),
            &head,
        )
        .await
        .map_err(ServiceError::internal)?;
        Ok(paths
            .into_iter()
            .filter(|p| sensitive.matches(p))
            .map(|p| String::from_utf8_lossy(&p).into_owned())
            .collect())
    }

    /// The `sensitive_paths` requirement: on a protected target, changes
    /// to sensitive paths need an approval by a person with the maintain
    /// role or higher, other than the author. Bots never satisfy it.
    async fn sensitive_requirement(
        &self,
        a: &RepoAccess,
        m: &MergeRequest,
        views: &[ReviewView],
    ) -> Result<Option<Requirement>> {
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
        let mut by = Vec::new();
        for v in views {
            let (Some(u), Some(name)) = (v.review.reviewer_id, v.review.reviewer.as_ref()) else {
                continue;
            };
            if v.review.verdict != "approve"
                || !v.counts
                || v.review.change_id.is_some()
                || m.author_id == Some(u)
            {
                continue;
            }
            let Some(user) = slath_db::users::get(&self.pool, u).await? else {
                continue;
            };
            if user.kind == UserKind::Bot {
                continue;
            }
            let role = self
                .repo_access_by_id(
                    &crate::notifications::user_subject(u, &user),
                    a.repo.id,
                    Action::RepoRead,
                )
                .await
                .ok()
                .and_then(|x| x.ctx.role);
            if role.is_some_and(|r| r >= Role::Maintain) {
                by.push(name.clone());
            }
        }
        Ok(Some(if by.is_empty() {
            unmet(
                "sensitive_paths",
                format!("Changes sensitive paths: {list}. Needs an approval by a maintainer."),
                "Ask a maintainer to review",
            )
        } else {
            met(
                "sensitive_paths",
                format!(
                    "Changes sensitive paths ({list}); approved by a maintainer: {}.",
                    by.join(", ")
                ),
            )
        }))
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
        if m.attention == "iterating" {
            reqs.push(unmet(
                "iterating",
                "The bot is still iterating.",
                "Mark it as ready",
            ));
        }
        let views = self.mr_reviews(m).await?;
        let mut approvers = Vec::new();
        let mut approvals = Vec::new();
        let mut blockers = Vec::new();
        let mut stale = 0;
        for v in views.iter().filter(|v| v.review.change_id.is_none()) {
            let (Some(u), Some(name)) = (v.review.reviewer_id, v.review.reviewer.clone()) else {
                continue;
            };
            // Neither the author nor the author's bots approve the author's work.
            if m.author_id == Some(u)
                || (m.author_id.is_some()
                    && slath_db::users::bot_owner_user(&self.pool, u).await? == m.author_id)
                || !self
                    .verdict_counts(a, u, eff.bots_count_as_approvers)
                    .await?
            {
                continue;
            }
            match (v.review.verdict.as_str(), v.counts) {
                ("approve", true) => {
                    approvers.push(name.clone());
                    approvals.push((&v.review, name));
                }
                ("request_changes", true) => blockers.push(name),
                ("approve", false) if v.stale => stale += 1,
                _ => {}
            }
        }
        let coverage = self.approval_coverage(m, &approvals).await?;
        // Each approver with what they had viewed: "alice (r3, 12 of 15
        // files viewed)".
        let named: Vec<String> = approvers
            .iter()
            .map(|n| {
                match coverage
                    .as_ref()
                    .and_then(|c| c.approvals.iter().find(|a| &a.reviewer == n))
                {
                    Some(a) => format!("{n} ({})", a.describe()),
                    None => n.clone(),
                }
            })
            .collect();
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
                if named.is_empty() {
                    "No approvals are required.".to_owned()
                } else {
                    format!("Approved by {}.", named.join(", "))
                },
            )
        } else {
            unmet(
                "approvals",
                if named.is_empty() {
                    format!("0 of {needed} required approvals{stale_note}.")
                } else {
                    format!(
                        "{} of {needed} required approvals{stale_note}: {}.",
                        named.len(),
                        named.join(", ")
                    )
                },
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
        if eff.protected
            && let Some(r) = self.sensitive_requirement(a, m, &views).await?
        {
            reqs.push(r);
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
            up_to_date,
            coverage,
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
            origin: slath_core::origin::Origin::Writer,
            created_at: time::OffsetDateTime::UNIX_EPOCH,
            change_id: None,
            change: None,
        }
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
