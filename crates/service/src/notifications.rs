// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Notifications about issues and merge requests: subscriptions, the
//! inbox, and email.
//!
//! An action queues one `notify.fanout` job. The job decides who hears
//! about it: people taking part in the thread (author, commenters,
//! assignees, and for merge requests reviewers), people mentioned in the
//! new text (directly or through a team), people asked to review, and
//! those watching the thread or the repository. The actor never notifies
//! themselves, and ignoring a thread silences it completely; ignoring a
//! repository leaves only what's aimed at them (mentions, assignments,
//! review requests). A draft merge request asks no one to review. Read access
//! is checked for every recipient when the job runs, again when the email
//! is sent, and again when the inbox is shown, so someone who lost access
//! hears nothing more and no longer sees older rows.

use std::collections::HashMap;

use slath_core::authz::{Action, Credential, Subject};
use slath_core::ids::{MergeRequestId, RepoId, UserId};
use slath_db::issues::Issue;
use slath_db::jobs::LeasedJob;
use slath_db::merge_requests::MergeRequest;
use slath_db::notifications::Notification;
use slath_db::worker::JobError;
use slath_markup::refs::{Ref, RefContext};

use crate::Services;
use crate::email::{Email, ReadCheck};
use crate::error::{Result, ServiceError};
use crate::repos::RepoAccess;

/// The job that works out and delivers the notifications of one action.
pub const NOTIFY_FANOUT: &str = "notify.fanout";
/// Inbox rows per page.
pub const INBOX_PAGE: i64 = 50;
/// Mentions that notify per text (the rest still link).
const MAX_MENTIONS: usize = 50;

/// What happened to an item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Activity {
    Opened,
    Closed,
    Reopened,
    Commented(uuid::Uuid),
    /// People were assigned.
    Assigned(Vec<UserId>),
}

impl Activity {
    fn name(&self) -> &'static str {
        match self {
            Self::Opened => "opened",
            Self::Closed => "closed",
            Self::Reopened => "reopened",
            Self::Commented(_) => "commented",
            Self::Assigned(_) => "assigned",
        }
    }
}

/// What happened to a merge request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MrActivity {
    Opened,
    Closed,
    Reopened,
    Merged,
    /// No longer a draft.
    Ready,
    /// A comment in the conversation.
    Commented(uuid::Uuid),
    /// A published comment in a review thread.
    ThreadComment(uuid::Uuid),
    /// A submitted review.
    Reviewed(uuid::Uuid),
    /// People were asked to review.
    ReviewRequested(Vec<UserId>),
    /// A new revision was pushed.
    Revision(i32),
    /// What became of merge when ready, for whoever set it: merged,
    /// cancelled, or failed, in words.
    AutoMerge {
        setter: UserId,
        outcome: String,
    },
}

impl MrActivity {
    fn name(&self) -> &'static str {
        match self {
            Self::Opened => "opened",
            Self::Closed => "closed",
            Self::Reopened => "reopened",
            Self::Merged => "merged",
            Self::Ready => "ready",
            Self::Commented(_) => "commented",
            Self::ThreadComment(_) => "thread_comment",
            Self::Reviewed(_) => "reviewed",
            Self::ReviewRequested(_) => "review_requested",
            Self::Revision(_) => "revision",
            Self::AutoMerge { .. } => "auto_merge",
        }
    }
}

/// Why someone hears about something, strongest first.
const REASONS: [&str; 9] = [
    "auto_merge_result",
    "assigned",
    "review_requested",
    "mentioned",
    "mr_ready",
    "author",
    "commented",
    "state_change",
    "watching",
];

/// Reasons aimed at the recipient, which get through an ignored repository.
const AIMED: [&str; 5] = [
    "auto_merge_result",
    "assigned",
    "review_requested",
    "mentioned",
    "mr_ready",
];

fn stronger(a: &str, b: &str) -> bool {
    let pos = |r: &str| {
        REASONS
            .iter()
            .position(|x| *x == r)
            .unwrap_or(REASONS.len())
    };
    pos(a) < pos(b)
}

/// Who hears about an action, each with their strongest reason.
#[derive(Default)]
struct Recipients(HashMap<UserId, &'static str>);

impl Recipients {
    fn add(&mut self, u: UserId, reason: &'static str) {
        let e = self.0.entry(u).or_insert(reason);
        if stronger(reason, e) {
            *e = reason;
        }
    }
}

/// The thread a fan-out delivers about.
struct Delivery<'a> {
    repo: &'a slath_db::repos::Repo,
    /// `issue` or `merge_request`.
    kind: &'static str,
    id: uuid::Uuid,
    /// The page under the repository, like `issues/4`.
    path: String,
    /// The email subject after `[ns/repo]`.
    subject: String,
    actor: Option<UserId>,
}

fn payload_uuid(p: &serde_json::Value, k: &str) -> Option<uuid::Uuid> {
    p.get(k)
        .and_then(serde_json::Value::as_str)
        .and_then(|s| s.parse().ok())
}

fn payload_users(p: &serde_json::Value, k: &str) -> Vec<UserId> {
    p.get(k)
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|v| v.as_str()?.parse().ok())
        .map(UserId::from_uuid)
        .collect()
}

fn retry(e: slath_db::DbError) -> JobError {
    JobError::Retry(e.to_string())
}

fn actor_of(subject: &Subject) -> Option<String> {
    match subject {
        Subject::User { id, .. } => Some(id.to_string()),
        _ => None,
    }
}

/// How a viewer is subscribed to a repository or thread.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Subscription {
    /// Nothing explicit (thread: participation decides).
    Default,
    Watching,
    Ignoring,
}

/// An inbox row as the viewer sees it.
#[derive(Debug, Clone)]
pub struct InboxItem {
    pub notification: Notification,
    pub repo: String,
    pub number: i32,
    pub title: String,
    pub state: String,
}

pub(crate) fn user_subject(id: UserId, r: &slath_db::users::User) -> Subject {
    Subject::User {
        id,
        kind: r.kind,
        state: r.state,
        is_site_admin: r.is_admin,
        credential: Credential::Session,
    }
}

fn session_user(subject: &Subject) -> Result<UserId> {
    match subject {
        Subject::User { id, .. } => Ok(*id),
        _ => Err(ServiceError::Unauthenticated),
    }
}

impl Services {
    /// Queues the notifications about an action on an issue. Failing to
    /// queue is logged; it never fails the action itself.
    pub(crate) async fn notify_item(
        &self,
        subject: &Subject,
        a: &RepoAccess,
        issue: &Issue,
        what: Activity,
    ) {
        let (comment, assigned) = match &what {
            Activity::Commented(c) => (Some(c.to_string()), Vec::new()),
            Activity::Assigned(users) => (None, users.iter().map(ToString::to_string).collect()),
            _ => (None, Vec::new()),
        };
        let actor = match subject {
            Subject::User { id, .. } => Some(*id),
            _ => None,
        };
        self.webhook_issue(actor, a, issue, &what).await;
        self.queue_fanout(serde_json::json!({
            "repo_id": a.repo.id.to_string(),
            "issue_id": issue.id.to_string(),
            "actor_id": actor_of(subject),
            "activity": what.name(),
            "comment_id": comment,
            "assigned": assigned,
        }))
        .await;
    }

    /// Queues the notifications about an action on a merge request, by
    /// `actor` (`None`: the system). Like [`Self::notify_item`], it never
    /// fails the action.
    pub(crate) async fn notify_mr(
        &self,
        actor: Option<UserId>,
        m: &MergeRequest,
        what: MrActivity,
    ) {
        let (item, users, revision) = match &what {
            MrActivity::Commented(c) | MrActivity::ThreadComment(c) | MrActivity::Reviewed(c) => {
                (Some(c.to_string()), Vec::new(), None)
            }
            MrActivity::ReviewRequested(u) => {
                (None, u.iter().map(ToString::to_string).collect(), None)
            }
            MrActivity::Revision(seq) => (None, Vec::new(), Some(*seq)),
            MrActivity::AutoMerge { setter, .. } => (None, vec![setter.to_string()], None),
            _ => (None, Vec::new(), None),
        };
        self.webhook_mr(actor, m, &what).await;
        let outcome = match &what {
            MrActivity::AutoMerge { outcome, .. } => Some(outcome.clone()),
            _ => None,
        };
        self.queue_fanout(serde_json::json!({
            "kind": "merge_request",
            "repo_id": m.repo_id.to_string(),
            "mr_id": m.id.to_string(),
            "actor_id": actor.map(|a| a.to_string()),
            "activity": what.name(),
            "item_id": item,
            "users": users,
            "revision": revision,
            "outcome": outcome,
        }))
        .await;
    }

    async fn queue_fanout(&self, payload: serde_json::Value) {
        let job = slath_db::jobs::NewJob::new("default", NOTIFY_FANOUT, payload)
            .dedupe_key(format!("{NOTIFY_FANOUT}:{}", uuid::Uuid::now_v7()));
        if let Err(e) = slath_db::jobs::enqueue(&self.pool, &job).await {
            tracing::warn!(error = %e, "queueing notifications failed");
        }
    }

    /// Whether a user can read the repository's issues (`kind` `issue`) or
    /// merge requests (`merge_request`) right now.
    pub(crate) async fn user_can_read(&self, user: UserId, repo: RepoId, kind: &str) -> bool {
        let Ok(Some(row)) = slath_db::users::get(&self.pool, user).await else {
            return false;
        };
        let them = user_subject(user, &row);
        self.repo_access_by_id(&them, repo, Action::RepoRead)
            .await
            .is_ok()
            && self.kind_enabled(repo, kind).await
    }

    /// Whether the repository has issues (`kind` `issue`) or merge requests
    /// (`merge_request`) turned on.
    async fn kind_enabled(&self, repo: RepoId, kind: &str) -> bool {
        match kind {
            "issue" => slath_db::repos::issues_enabled(&self.pool, repo)
                .await
                .unwrap_or(false),
            "merge_request" => slath_db::repos::mr_settings(&self.pool, repo)
                .await
                .is_ok_and(|s| s.0),
            _ => false,
        }
    }

    /// The `notify.fanout` job.
    pub async fn run_fanout(&self, job: &LeasedJob) -> std::result::Result<(), JobError> {
        if job.payload.get("kind").and_then(serde_json::Value::as_str) == Some("merge_request") {
            return self.run_mr_fanout(job).await;
        }
        let uuid_of = |k: &str| payload_uuid(&job.payload, k);
        let bad = |k: &str| JobError::Permanent(format!("payload has no valid {k}"));
        let repo_id = RepoId::from_uuid(uuid_of("repo_id").ok_or_else(|| bad("repo_id"))?);
        let issue_id = uuid_of("issue_id").ok_or_else(|| bad("issue_id"))?;
        let actor = uuid_of("actor_id").map(UserId::from_uuid);
        let activity = job
            .payload
            .get("activity")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("");

        let Some(repo) = slath_db::repos::get(&self.pool, repo_id)
            .await
            .map_err(retry)?
        else {
            return Ok(());
        };
        let Some((_, number)) = slath_db::notifications::issue_number(&self.pool, issue_id)
            .await
            .map_err(retry)?
        else {
            return Ok(());
        };
        let Some(issue) = slath_db::issues::get(&self.pool, repo_id, number)
            .await
            .map_err(retry)?
        else {
            return Ok(());
        };
        let comment = match uuid_of("comment_id") {
            Some(c) => slath_db::issues::comment(&self.pool, c)
                .await
                .map_err(retry)?
                .filter(|c| c.deleted_at.is_none() && c.hidden_at.is_none()),
            None => None,
        };
        if activity == "commented" && comment.is_none() {
            return Ok(());
        }
        let ctx = RefContext {
            ns: repo.namespace.clone(),
            repo: repo.name.clone(),
        };

        // Who, and why (the strongest reason wins).
        let mut who = Recipients::default();
        self.add_watchers(&mut who, repo_id, "issue", issue.id)
            .await
            .map_err(retry)?;
        for u in slath_db::notifications::commenters(&self.pool, "issue", issue.id)
            .await
            .map_err(retry)?
        {
            who.add(u, "commented");
        }
        if let Some(a) = issue.author_id {
            who.add(a, "author");
        }
        let assignees = slath_db::labels::assignees(&self.pool, &[issue.id])
            .await
            .map_err(retry)?;
        let assigned_now = payload_users(&job.payload, "assigned");
        for (_, u, _) in &assignees {
            who.add(
                *u,
                if assigned_now.contains(u) {
                    "assigned"
                } else {
                    "commented"
                },
            );
        }
        for u in &assigned_now {
            who.add(*u, "assigned");
        }
        // Mentions in the new text.
        let text = match (&comment, activity) {
            (Some(c), _) => c.body.as_str(),
            (None, "opened") => issue.body.as_str(),
            _ => "",
        };
        for u in self.mentioned_users(text, &ctx).await.map_err(retry)? {
            who.add(u, "mentioned");
        }

        let actor_name = self.actor_name(actor).await.map_err(retry)?;
        let summary = match (activity, &comment) {
            ("commented", Some(c)) => format!("{actor_name} commented:\n\n{}", c.body),
            ("opened", _) => format!("{actor_name} opened this issue:\n\n{}", issue.body),
            ("closed", _) => format!("{actor_name} closed this issue."),
            ("reopened", _) => format!("{actor_name} reopened this issue."),
            ("assigned", _) => format!("{actor_name} changed the assignees."),
            _ => format!("{actor_name} updated this issue."),
        };
        let d = Delivery {
            repo: &repo,
            kind: "issue",
            id: issue.id,
            path: format!("issues/{}", issue.number),
            subject: format!("{} (#{})", issue.title, issue.number),
            actor,
        };
        self.deliver_notifications(&d, who, |_| summary.clone())
            .await
    }

    /// The fan-out of an action on a merge request. Taking part means
    /// writing in it (conversation, review threads, reviews), being
    /// assigned, or being asked to review; a draft asks no one to review
    /// yet, so its requested reviewers hear nothing until it's ready.
    async fn run_mr_fanout(&self, job: &LeasedJob) -> std::result::Result<(), JobError> {
        let uuid_of = |k: &str| payload_uuid(&job.payload, k);
        let bad = |k: &str| JobError::Permanent(format!("payload has no valid {k}"));
        let repo_id = RepoId::from_uuid(uuid_of("repo_id").ok_or_else(|| bad("repo_id"))?);
        let mr_id = uuid_of("mr_id").ok_or_else(|| bad("mr_id"))?;
        let actor = uuid_of("actor_id").map(UserId::from_uuid);
        let activity = job
            .payload
            .get("activity")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("");
        let Some(repo) = slath_db::repos::get(&self.pool, repo_id)
            .await
            .map_err(retry)?
        else {
            return Ok(());
        };
        let Some(m) =
            slath_db::merge_requests::get_by_id(&self.pool, MergeRequestId::from_uuid(mr_id))
                .await
                .map_err(retry)?
                .filter(|m| m.repo_id == repo_id)
        else {
            return Ok(());
        };
        let actor_name = self.actor_name(actor).await.map_err(retry)?;
        let item = uuid_of("item_id");

        // What happened, in words, and the new text (for mentions).
        let (summary, text) = match activity {
            "opened" => (
                format!(
                    "{actor_name} opened this merge request:\n\n{}",
                    m.description
                ),
                m.description.clone(),
            ),
            "closed" => (
                format!("{actor_name} closed this merge request."),
                String::new(),
            ),
            "reopened" => (
                format!("{actor_name} reopened this merge request."),
                String::new(),
            ),
            "merged" => (
                format!(
                    "{actor_name} merged this merge request into {}.",
                    m.target_branch
                ),
                String::new(),
            ),
            "ready" => (
                format!("{actor_name} marked this merge request ready for review."),
                String::new(),
            ),
            "review_requested" => (
                format!("{actor_name} asked you to review this merge request."),
                String::new(),
            ),
            "auto_merge" => (
                job.payload
                    .get("outcome")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("Merge when ready ended.")
                    .to_owned(),
                String::new(),
            ),
            "revision" => {
                let seq = job
                    .payload
                    .get("revision")
                    .and_then(serde_json::Value::as_i64)
                    .unwrap_or(i64::from(m.revisions));
                (
                    format!("{actor_name} pushed revision {seq}."),
                    String::new(),
                )
            }
            "commented" => {
                let Some(c) = (match item {
                    Some(c) => slath_db::issues::comment(&self.pool, c)
                        .await
                        .map_err(retry)?
                        .filter(|c| c.deleted_at.is_none() && c.hidden_at.is_none()),
                    None => None,
                }) else {
                    return Ok(());
                };
                (format!("{actor_name} commented:\n\n{}", c.body), c.body)
            }
            "thread_comment" => {
                let comments = slath_db::mr_threads::comments(&self.pool, m.id)
                    .await
                    .map_err(retry)?;
                let Some(c) = comments
                    .into_iter()
                    .find(|c| Some(c.id) == item && !c.pending && c.deleted_at.is_none())
                else {
                    return Ok(());
                };
                let path = slath_db::mr_threads::threads(&self.pool, m.id)
                    .await
                    .map_err(retry)?
                    .into_iter()
                    .find(|t| t.id == c.thread_id)
                    .and_then(|t| t.anchor.path);
                let on = path.map(|p| format!(" on {p}")).unwrap_or_default();
                (format!("{actor_name} commented{on}:\n\n{}", c.body), c.body)
            }
            "reviewed" => {
                let Some(r) = slath_db::mr_reviews::list(&self.pool, m.id)
                    .await
                    .map_err(retry)?
                    .into_iter()
                    .find(|r| Some(r.id) == item)
                else {
                    return Ok(());
                };
                let did = match r.verdict.as_str() {
                    "approve" => "approved",
                    "request_changes" => "requested changes",
                    _ => "reviewed",
                };
                let body = if r.body.is_empty() {
                    ".".to_owned()
                } else {
                    format!(":\n\n{}", r.body)
                };
                (format!("{actor_name} {did}{body}"), r.body)
            }
            _ => return Ok(()),
        };

        let mut who = Recipients::default();
        if activity == "auto_merge" {
            // Only whoever set it.
            for u in payload_users(&job.payload, "users") {
                who.add(u, "auto_merge_result");
            }
        } else if activity == "review_requested" {
            // Only the people asked; never while it's a draft.
            if !m.draft && m.state == "open" {
                let asked = payload_users(&job.payload, "users");
                let now = slath_db::notifications::requested_reviewers(&self.pool, m.id)
                    .await
                    .map_err(retry)?;
                for u in asked.into_iter().filter(|u| now.contains(u)) {
                    who.add(u, "review_requested");
                }
            }
        } else {
            self.add_watchers(&mut who, repo_id, "merge_request", *m.id.as_uuid())
                .await
                .map_err(retry)?;
            for u in
                slath_db::notifications::commenters(&self.pool, "merge_request", *m.id.as_uuid())
                    .await
                    .map_err(retry)?
            {
                who.add(u, "commented");
            }
            for u in slath_db::notifications::mr_participants(&self.pool, m.id)
                .await
                .map_err(retry)?
            {
                who.add(u, "commented");
            }
            if let Some(a) = m.author_id {
                who.add(a, "author");
            }
            if matches!(activity, "opened" | "ready") && !m.draft {
                let reason = if activity == "opened" {
                    "review_requested"
                } else {
                    "mr_ready"
                };
                for u in slath_db::notifications::requested_reviewers(&self.pool, m.id)
                    .await
                    .map_err(retry)?
                {
                    who.add(u, reason);
                }
            }
            let ctx = RefContext {
                ns: repo.namespace.clone(),
                repo: repo.name.clone(),
            };
            for u in self.mentioned_users(&text, &ctx).await.map_err(retry)? {
                who.add(u, "mentioned");
            }
        }
        let d = Delivery {
            repo: &repo,
            kind: "merge_request",
            id: *m.id.as_uuid(),
            path: format!("merge-requests/{}", m.number),
            subject: format!("{} (!{})", m.title, m.number),
            actor,
        };
        let opened = activity == "opened";
        self.deliver_notifications(&d, who, |reason| match reason {
            "review_requested" if opened => format!(
                "{actor_name} asked you to review this merge request:\n\n{}",
                m.description
            ),
            "mr_ready" => {
                format!("{actor_name} marked this merge request ready; you're asked to review it.")
            }
            _ => summary.clone(),
        })
        .await
    }

    /// Adds who watches the repository or the thread.
    async fn add_watchers(
        &self,
        who: &mut Recipients,
        repo: RepoId,
        kind: &str,
        id: uuid::Uuid,
    ) -> std::result::Result<(), slath_db::DbError> {
        for u in
            slath_db::notifications::subscribers(&self.pool, "repo", *repo.as_uuid(), "watching")
                .await?
        {
            who.add(u, "watching");
        }
        for u in slath_db::notifications::subscribers(&self.pool, kind, id, "watching").await? {
            who.add(u, "watching");
        }
        Ok(())
    }

    async fn actor_name(
        &self,
        actor: Option<UserId>,
    ) -> std::result::Result<String, slath_db::DbError> {
        Ok(match actor {
            Some(a) => slath_db::notifications::recipient(&self.pool, a)
                .await?
                .map_or_else(|| "Someone".to_owned(), |r| r.name),
            None => "Someone".to_owned(),
        })
    }

    /// Bumps the inbox row of everyone in `who` and emails them, skipping
    /// the actor, anyone ignoring the thread (or the repository, except for
    /// reasons aimed at them), and anyone who can't read it now.
    async fn deliver_notifications(
        &self,
        d: &Delivery<'_>,
        who: Recipients,
        summary: impl Fn(&str) -> String,
    ) -> std::result::Result<(), JobError> {
        let repo_id = d.repo.id;
        let ns = &d.repo.namespace;
        let name = &d.repo.name;
        let ignoring_thread =
            slath_db::notifications::subscribers(&self.pool, d.kind, d.id, "ignoring")
                .await
                .map_err(retry)?;
        let ignoring_repo = slath_db::notifications::subscribers(
            &self.pool,
            "repo",
            *repo_id.as_uuid(),
            "ignoring",
        )
        .await
        .map_err(retry)?;
        let base = self
            .config
            .server
            .public_url
            .origin()
            .trim_end_matches('/')
            .to_owned();
        let url = format!("{base}/{ns}/{name}/{}", d.path);
        for (user, reason) in who.0 {
            if Some(user) == d.actor || ignoring_thread.contains(&user) {
                continue;
            }
            if ignoring_repo.contains(&user) && !AIMED.contains(&reason) {
                continue;
            }
            if !self.user_can_read(user, repo_id, d.kind).await {
                continue;
            }
            let Some(r) = slath_db::notifications::recipient(&self.pool, user)
                .await
                .map_err(retry)?
            else {
                continue;
            };
            slath_db::notifications::bump(&self.pool, user, repo_id, d.kind, d.id, reason)
                .await
                .map_err(retry)?;
            if !(self.email_enabled() && r.email_verified && r.email_notifications) {
                continue;
            }
            let Some(to) = r.email else { continue };
            let unsubscribe = format!(
                "{base}/-/unsubscribe/{}",
                self.unsubscribe_token(user, d.kind, d.id)
            );
            let text = format!(
                "{}\n\n--\nView it on Slath: {url}\nYou are receiving this because of: {}.\nUnsubscribe from this thread: {unsubscribe}\n",
                summary(reason),
                reason.replace('_', " ")
            );
            let email = Email {
                to,
                subject: format!("[{ns}/{name}] {}", d.subject),
                text,
                html: None,
                thread: Some(format!("{}/{}", d.kind.replace('_', "-"), d.id)),
                list_id: Some(format!(
                    "{ns}/{name}.{}",
                    self.config.server.public_url.host()
                )),
                unsubscribe: Some(unsubscribe),
                read_check: Some(ReadCheck {
                    user: *user.as_uuid(),
                    repo: *repo_id.as_uuid(),
                    merge_request: d.kind == "merge_request",
                }),
            };
            if let Err(e) = self.queue_email(&email).await {
                return Err(JobError::Retry(e.to_string()));
            }
        }
        Ok(())
    }

    /// Users mentioned in a text: `@user`, and members of `@org/team`.
    async fn mentioned_users(
        &self,
        text: &str,
        ctx: &RefContext,
    ) -> std::result::Result<Vec<UserId>, slath_db::DbError> {
        if text.is_empty() {
            return Ok(Vec::new());
        }
        let html = slath_markup::render_with_refs(
            text,
            slath_markup::MarkdownOptions::default(),
            Some(ctx),
        );
        let mut out = Vec::new();
        for r in slath_markup::refs::references(&html)
            .into_iter()
            .take(MAX_MENTIONS)
        {
            match r {
                Ref::Mention { name } => {
                    if let Some((_, _, slath_db::orgs::NamespaceKind::User(u))) =
                        slath_db::orgs::resolve_namespace(&self.pool, &name).await?
                    {
                        out.push(u);
                    }
                }
                Ref::Team { org, team } => {
                    if let Some((_, _, slath_db::orgs::NamespaceKind::Org(o))) =
                        slath_db::orgs::resolve_namespace(&self.pool, &org).await?
                        && let Some(t) = slath_db::teams::find(&self.pool, o, &team).await?
                    {
                        out.extend(
                            slath_db::teams::members(&self.pool, t.id)
                                .await?
                                .into_iter()
                                .map(|(u, _)| u),
                        );
                    }
                }
                _ => {}
            }
        }
        Ok(out)
    }

    /// The token in unsubscribe links: user, thread, and an HMAC.
    #[must_use]
    pub fn unsubscribe_token(&self, user: UserId, kind: &str, subject: uuid::Uuid) -> String {
        let payload = format!("{user}.{kind}.{subject}");
        let mac = self.unsubscribe_keys.token(&payload);
        format!("{payload}.{mac}")
    }

    /// Checks an unsubscribe token: `(user, kind, subject)`.
    fn check_unsubscribe(&self, token: &str) -> Result<(UserId, String, uuid::Uuid)> {
        let mut parts = token.splitn(4, '.');
        let (Some(u), Some(k), Some(s), Some(mac)) =
            (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            return Err(ServiceError::NotFound);
        };
        if !self.unsubscribe_keys.verify(&format!("{u}.{k}.{s}"), mac) {
            return Err(ServiceError::NotFound);
        }
        let user: UserId = u.parse().map_err(|_| ServiceError::NotFound)?;
        let subject: uuid::Uuid = s.parse().map_err(|_| ServiceError::NotFound)?;
        if k != "issue" && k != "merge_request" {
            return Err(ServiceError::NotFound);
        }
        Ok((user, k.to_owned(), subject))
    }

    /// The repository and number of an issue or merge request.
    async fn thread_number(&self, kind: &str, id: uuid::Uuid) -> Result<Option<(RepoId, i32)>> {
        Ok(match kind {
            "issue" => slath_db::notifications::issue_number(&self.pool, id).await?,
            "merge_request" => slath_db::notifications::mr_number(&self.pool, id).await?,
            _ => None,
        })
    }

    /// What an unsubscribe link refers to, as `ns/repo#n` or `ns/repo!n`
    /// (or `None` if the recipient can no longer read it).
    pub async fn unsubscribe_target(&self, token: &str) -> Result<Option<String>> {
        let (user, kind, subject) = self.check_unsubscribe(token)?;
        let Some((repo, number)) = self.thread_number(&kind, subject).await? else {
            return Ok(None);
        };
        if !self.user_can_read(user, repo, &kind).await {
            return Ok(None);
        }
        let mark = if kind == "issue" { '#' } else { '!' };
        let r = slath_db::repos::get(&self.pool, repo).await?;
        Ok(r.map(|r| format!("{}/{}{mark}{number}", r.namespace, r.name)))
    }

    /// Follows an unsubscribe link: the user ignores the thread from now on.
    /// The signed token is the authorization; no session is needed.
    pub async fn unsubscribe(&self, token: &str) -> Result<()> {
        let (user, kind, subject) = self.check_unsubscribe(token)?;
        let Some((repo, _)) = self.thread_number(&kind, subject).await? else {
            return Ok(());
        };
        slath_db::notifications::set_subscription(
            &self.pool,
            user,
            &kind,
            subject,
            repo,
            Some("ignoring"),
        )
        .await?;
        slath_db::notifications::mark_thread_read(&self.pool, user, &kind, subject).await?;
        Ok(())
    }

    /// The subject's subscription to a repository.
    pub async fn repo_subscription(&self, subject: &Subject, a: &RepoAccess) -> Subscription {
        let Subject::User { id, .. } = subject else {
            return Subscription::Default;
        };
        match slath_db::notifications::subscription(&self.pool, *id, "repo", *a.repo.id.as_uuid())
            .await
        {
            Ok(Some(s)) if s == "watching" => Subscription::Watching,
            Ok(Some(s)) if s == "ignoring" => Subscription::Ignoring,
            _ => Subscription::Default,
        }
    }

    /// Watches, ignores, or resets (`Default`) a repository the subject can read.
    pub async fn set_repo_subscription(
        &self,
        subject: &Subject,
        ns: &str,
        repo: &str,
        to: Subscription,
    ) -> Result<()> {
        let user = crate::tokens::user_with_scope(subject, slath_core::authz::Scope::UserWrite)?;
        let a = self
            .repo_access(subject, ns, repo, Action::RepoRead)
            .await?;
        let state = match to {
            Subscription::Default => None,
            Subscription::Watching => Some("watching"),
            Subscription::Ignoring => Some("ignoring"),
        };
        slath_db::notifications::set_subscription(
            &self.pool,
            user,
            "repo",
            *a.repo.id.as_uuid(),
            a.repo.id,
            state,
        )
        .await?;
        Ok(())
    }

    /// The subject's subscription to an issue thread.
    pub async fn issue_subscription(&self, subject: &Subject, issue: &Issue) -> Subscription {
        let Subject::User { id, .. } = subject else {
            return Subscription::Default;
        };
        match slath_db::notifications::subscription(&self.pool, *id, "issue", issue.id).await {
            Ok(Some(s)) if s == "watching" => Subscription::Watching,
            Ok(Some(s)) if s == "ignoring" => Subscription::Ignoring,
            _ => Subscription::Default,
        }
    }

    /// Whether the subject currently gets notifications about an issue: an
    /// explicit choice wins; otherwise taking part (author, commenter,
    /// assignee) or watching the repository subscribes them.
    pub async fn issue_notified(&self, subject: &Subject, a: &RepoAccess, issue: &Issue) -> bool {
        let Subject::User { id, .. } = subject else {
            return false;
        };
        match self.issue_subscription(subject, issue).await {
            Subscription::Watching => return true,
            Subscription::Ignoring => return false,
            Subscription::Default => {}
        }
        if issue.author_id == Some(*id) {
            return true;
        }
        let pool = &self.pool;
        if slath_db::notifications::commenters(pool, "issue", issue.id)
            .await
            .is_ok_and(|c| c.contains(id))
        {
            return true;
        }
        if slath_db::labels::assignees(pool, &[issue.id])
            .await
            .is_ok_and(|v| v.iter().any(|(_, u, _)| u == id))
        {
            return true;
        }
        self.repo_subscription(subject, a).await == Subscription::Watching
    }

    /// Subscribes to or ignores an issue thread the subject can read.
    pub async fn set_issue_subscription(
        &self,
        subject: &Subject,
        ns: &str,
        repo: &str,
        number: i32,
        to: Subscription,
    ) -> Result<()> {
        let user = crate::tokens::user_with_scope(subject, slath_core::authz::Scope::UserWrite)?;
        let (a, issue) = self.issue(subject, ns, repo, number).await?;
        let state = match to {
            Subscription::Default => None,
            Subscription::Watching => Some("watching"),
            Subscription::Ignoring => Some("ignoring"),
        };
        slath_db::notifications::set_subscription(
            &self.pool, user, "issue", issue.id, a.repo.id, state,
        )
        .await?;
        Ok(())
    }

    /// Marks the subject's notification about an issue read (on viewing it).
    pub async fn mark_issue_read(&self, subject: &Subject, issue: &Issue) {
        if let Subject::User { id, .. } = subject {
            let _ =
                slath_db::notifications::mark_thread_read(&self.pool, *id, "issue", issue.id).await;
        }
    }

    /// A page of the subject's inbox. Rows about things the subject can no
    /// longer read are left out.
    pub async fn inbox(
        &self,
        subject: &Subject,
        unread_only: bool,
        offset: i64,
        limit: i64,
    ) -> Result<Vec<InboxItem>> {
        let user = crate::tokens::user_with_scope(subject, slath_core::authz::Scope::UserRead)?;
        let rows =
            slath_db::notifications::inbox(&self.pool, user, unread_only, offset, limit).await?;
        let mut out = Vec::with_capacity(rows.len());
        for n in rows {
            let Ok(a) = self
                .repo_access_by_id(subject, n.repo_id, Action::RepoRead)
                .await
            else {
                continue;
            };
            let Some((repo, number)) = self.thread_number(&n.subject_kind, n.subject_id).await?
            else {
                continue;
            };
            if repo != a.repo.id || !self.kind_enabled(repo, &n.subject_kind).await {
                continue;
            }
            let found = if n.subject_kind == "issue" {
                slath_db::issues::titles(&self.pool, a.repo.id, &[number])
                    .await?
                    .pop()
                    .map(|(_, t, s)| (t, s))
            } else {
                slath_db::merge_requests::get(&self.pool, a.repo.id, number)
                    .await?
                    .map(|m| (m.title, m.state))
            };
            let Some((title, state)) = found else {
                continue;
            };
            out.push(InboxItem {
                repo: format!("{}/{}", a.repo.namespace, a.repo.name),
                number,
                title,
                state,
                notification: n,
            });
        }
        Ok(out)
    }

    /// Marks one notification read or unread.
    pub async fn set_notification_unread(
        &self,
        subject: &Subject,
        id: uuid::Uuid,
        unread: Option<bool>,
    ) -> Result<()> {
        let user = crate::tokens::user_with_scope(subject, slath_core::authz::Scope::UserWrite)?;
        let unread = unread.ok_or_else(|| {
            ServiceError::invalid("unread", "required", "Set unread to true or false.")
        })?;
        if slath_db::notifications::set_unread(&self.pool, user, id, unread).await? {
            Ok(())
        } else {
            Err(ServiceError::NotFound)
        }
    }

    pub async fn mark_all_notifications_read(&self, subject: &Subject) -> Result<()> {
        let user = crate::tokens::user_with_scope(subject, slath_core::authz::Scope::UserWrite)?;
        slath_db::notifications::mark_all_read(&self.pool, user).await?;
        Ok(())
    }

    /// Whether the subject gets notifications by email.
    pub async fn email_notifications(&self, subject: &Subject) -> Result<bool> {
        let user = session_user(subject)?;
        Ok(slath_db::notifications::email_notifications(&self.pool, user).await?)
    }

    pub async fn set_email_notifications(&self, subject: &Subject, on: bool) -> Result<()> {
        let user = crate::tokens::user_with_scope(subject, slath_core::authz::Scope::UserWrite)?;
        slath_db::notifications::set_email_notifications(&self.pool, user, on).await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reasons_are_ordered() {
        assert!(stronger("assigned", "watching"));
        assert!(stronger("mentioned", "author"));
        assert!(!stronger("watching", "commented"));
    }
}
