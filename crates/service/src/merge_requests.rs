// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Merge requests: the compare page, creation, the list, the conversation
//! (comments, events, revisions), closing and reopening, draft and ready,
//! and the revisions a push to the source creates (`mr.sync`).
//!
//! An MR's source is a branch of the same repository or of another
//! repository in its fork network. A fork's branch reaches the target only
//! through `mr.fetch`, which copies exactly the source's history into the
//! reserved ref `refs/slath/mr/<id>/incoming`; until an MR exists the
//! target holds nothing of the fork. Numbers are shared with issues. Permissions follow the matrix (spec/04): `mr.create` opens,
//! authors edit and close their own, `mr.triage` edits and closes any.

use slath_core::authz::{Action, Subject};
use slath_core::ids::{MergeRequestId, RepoId, UserId};
use slath_db::merge_requests::{MergeRequest, NewMergeRequest, NewRevision, Revision};
use slath_git::ObjectId;
use slath_git::args::RefName;
use slath_git::browse::LogEntry;

use crate::Services;
use crate::error::{Result, ServiceError};
use crate::notifications::MrActivity;
use crate::repos::RepoAccess;

/// The job that records a new revision when a source branch moves.
pub const MR_SYNC: &str = "mr.sync";
/// The job that fetches a fork's source branch into the target and then
/// records the revision like `mr.sync`.
pub const MR_FETCH: &str = "mr.fetch";
/// Longest title in characters.
const MAX_TITLE: usize = 256;
/// The template a repository can provide for new descriptions.
pub const TEMPLATE_PATH: &str = ".slath/merge_request_template.md";

/// What a viewer may do with a merge request.
#[derive(Debug, Clone, Copy, Default)]
pub struct MrPerms {
    pub edit: bool,
    pub close: bool,
    pub comment: bool,
    pub triage: bool,
    /// Hide and delete other people's comments.
    pub moderate: bool,
}

/// One entry of the conversation.
#[derive(Debug, Clone)]
pub enum MrTimelineItem {
    Comment(crate::issues::TimelineItem),
    Event(slath_db::issues::Event),
    /// A `referenced` event and its source, which the viewer can read.
    Referenced(slath_db::issues::Event, crate::cross_refs::RefSource),
    Revision {
        revision: Revision,
        /// The previous revision's head, for "compare with previous".
        previous_head: Option<String>,
    },
    Review(crate::mr_reviews::ReviewView),
    Thread(crate::mr_threads::ThreadView),
}

/// A compare of a source branch against a target branch.
#[derive(Debug, Clone)]
pub struct Compare {
    /// `ns/name` of the source repository when it isn't the target.
    pub source_repo: Option<String>,
    /// The source is less visible than the target: opening the MR makes
    /// its commits readable there and needs the author's confirmation.
    pub publishes_source: bool,
    pub source_branch: String,
    pub target_branch: String,
    pub source_head: ObjectId,
    pub target_head: ObjectId,
    pub merge_base: Option<ObjectId>,
    /// Commits on the source and not on the target, oldest first.
    pub commits: Vec<LogEntry>,
    /// More commits than shown.
    pub more_commits: bool,
    /// The raw unified diff from the merge base and whether it was cut.
    pub diff: Vec<u8>,
    pub diff_truncated: bool,
    /// The open MR for this source and target, if one exists.
    pub existing: Option<i32>,
    /// The repository's description template, if it has one.
    pub template: Option<String>,
}

/// What creating a merge request takes.
#[derive(Debug, Clone, Default)]
pub struct NewMr {
    /// `ns/name` of a repository in the target's fork network; empty: the
    /// target itself.
    pub source_repo: String,
    pub source_branch: String,
    pub target_branch: String,
    pub title: String,
    pub description: String,
    pub draft: bool,
    pub reviewers: Vec<String>,
    pub assignees: Vec<String>,
    pub labels: Vec<String>,
    pub milestone: Option<String>,
    /// `None`: the repository's default.
    pub delete_source_on_merge: Option<bool>,
    /// Fork MRs: whoever can write to the target may push to the source
    /// branch while the MR is open. `None`: allowed.
    pub allow_maintainer_edits: Option<bool>,
    /// The author confirms that a less visible source's commits become
    /// readable to the target's readers.
    pub publish_source: bool,
}

fn subject_user(subject: &Subject) -> Option<UserId> {
    match subject {
        Subject::User { id, .. } => Some(*id),
        _ => None,
    }
}

fn check_title(title: &str) -> Result<String> {
    let t = title.trim();
    if t.is_empty() {
        return Err(ServiceError::invalid("title", "required", "Enter a title."));
    }
    if t.chars().count() > MAX_TITLE || t.chars().any(char::is_control) {
        return Err(ServiceError::invalid(
            "title",
            "invalid",
            format!("Use at most {MAX_TITLE} characters, without control characters."),
        ));
    }
    Ok(t.to_owned())
}

fn branch_ref(field: &str, name: &str) -> Result<RefName> {
    RefName::branch(name.trim().as_bytes())
        .map_err(|_| ServiceError::invalid(field, "invalid", "Choose an existing branch."))
}

fn mr_from(m: &MergeRequest) -> crate::cross_refs::RefFrom {
    crate::cross_refs::RefFrom {
        kind: "merge_request",
        number: m.number,
    }
}

impl Services {
    /// The repository with `action` allowed, if merge requests are on there.
    pub(crate) async fn mrs_access(
        &self,
        subject: &Subject,
        ns: &str,
        repo: &str,
        action: Action,
    ) -> Result<RepoAccess> {
        let a = self.repo_access(subject, ns, repo, action).await?;
        if !slath_db::repos::mr_settings(&self.pool, a.repo.id).await?.0 {
            return Err(ServiceError::NotFound);
        }
        Ok(a)
    }

    /// The source repository `name` (`ns/name`) of a merge request into
    /// `a`, readable by the subject and in the same fork network; `None`
    /// when it's the target itself.
    pub async fn mr_source(
        &self,
        subject: &Subject,
        a: &RepoAccess,
        name: Option<&str>,
    ) -> Result<Option<RepoAccess>> {
        let Some(name) = name.map(str::trim).filter(|n| !n.is_empty()) else {
            return Ok(None);
        };
        let unknown = || {
            ServiceError::invalid(
                "source_repo",
                "unknown",
                "Choose a repository in this repository's fork network.",
            )
        };
        let (ns, repo) = name.split_once('/').ok_or_else(unknown)?;
        let src = match self.repo_access(subject, ns, repo, Action::RepoRead).await {
            Ok(src) => src,
            Err(ServiceError::NotFound | ServiceError::Forbidden) => return Err(unknown()),
            Err(e) => return Err(e),
        };
        if src.repo.id == a.repo.id {
            return Ok(None);
        }
        if !slath_db::forks::same_network(&self.pool, src.repo.id, a.repo.id).await? {
            return Err(unknown());
        }
        Ok(Some(src))
    }

    /// The repositories of `a`'s fork network the subject can push to and
    /// open merge requests from (`ns/name`), for the compare page.
    pub async fn mr_source_choices(
        &self,
        subject: &Subject,
        a: &RepoAccess,
    ) -> Result<Vec<String>> {
        let Some(me) = subject_user(subject) else {
            return Ok(Vec::new());
        };
        let mut out = Vec::new();
        for id in slath_db::forks::network_candidates(&self.pool, a.repo.id, me).await? {
            if let Ok(r) = self.repo_access_by_id(subject, id, Action::RepoPush).await {
                out.push(format!("{}/{}", r.repo.namespace, r.repo.name));
            }
        }
        Ok(out)
    }

    /// Whether the subject may turn edits by maintainers on or off: a fork
    /// merge request, and push permission on its source.
    pub async fn may_set_maintainer_edits(&self, subject: &Subject, m: &MergeRequest) -> bool {
        match m.source_repo_id.filter(|s| *s != m.repo_id) {
            Some(src) => self
                .repo_access_by_id(subject, src, Action::RepoPush)
                .await
                .is_ok(),
            None => false,
        }
    }

    /// `ns/name` of a merge request's source repository (shown with the
    /// MR to everyone who can read it: opening the MR published its
    /// branch), or `None` once the source is deleted.
    pub async fn mr_source_name(&self, m: &MergeRequest) -> Result<Option<String>> {
        let Some(id) = m.source_repo_id else {
            return Ok(None);
        };
        Ok(slath_db::repos::get(&self.pool, id)
            .await?
            .map(|r| format!("{}/{}", r.namespace, r.name)))
    }

    async fn branch_head(&self, repo: RepoId, name: &RefName) -> Result<Option<ObjectId>> {
        slath_git::refs::read(&self.runner, &self.repo_path(repo), name)
            .await
            .map_err(ServiceError::internal)
    }

    /// The compare of `source` (a branch of `source_repo`, default: this
    /// repository) against `target` (default: the default branch), as the
    /// compare page and the create form show it.
    pub async fn compare(
        &self,
        subject: &Subject,
        ns: &str,
        repo: &str,
        (source_repo, source): (Option<&str>, &str),
        target: Option<&str>,
        opts: slath_git::browse::DiffOptions,
    ) -> Result<(RepoAccess, Compare)> {
        let a = self.mrs_access(subject, ns, repo, Action::RepoRead).await?;
        let src = self.mr_source(subject, &a, source_repo).await?;
        let src_id = src.as_ref().map_or(a.repo.id, |s| s.repo.id);
        let target = target
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .unwrap_or(&a.repo.default_branch)
            .to_owned();
        let source = source.trim().to_owned();
        let source_ref = branch_ref("source_branch", &source)?;
        let target_ref = branch_ref("target_branch", &target)?;
        let source_head = self
            .branch_head(src_id, &source_ref)
            .await?
            .ok_or_else(|| {
                ServiceError::invalid("source_branch", "missing", "This branch doesn't exist.")
            })?;
        let target_head = self
            .branch_head(a.repo.id, &target_ref)
            .await?
            .ok_or_else(|| {
                ServiceError::invalid("target_branch", "missing", "This branch doesn't exist.")
            })?;
        let dir = self.repo_path(a.repo.id);
        // A fork's objects are read in place; nothing is copied before an
        // MR exists.
        let runner = match &src {
            Some(s) => self
                .runner
                .reading_objects_of(&self.repo_path(s.repo.id))
                .map_err(ServiceError::internal)?,
            None => self.runner.clone(),
        };
        let merge_base = slath_git::browse::merge_base(&runner, &dir, &target_head, &source_head)
            .await
            .map_err(ServiceError::internal)?;
        let max = self.config.limits.review_max_commits;
        let mut commits =
            slath_git::browse::log_range(&runner, &dir, Some(&target_head), &source_head, max + 1)
                .await
                .map_err(ServiceError::internal)?;
        let more_commits = commits.len() > max;
        if more_commits {
            commits.remove(0);
        }
        let (diff, diff_truncated) = slath_git::browse::diff(
            &runner,
            &dir,
            merge_base.as_ref(),
            &source_head,
            opts,
            64 * 1024 * 1024,
        )
        .await
        .map_err(ServiceError::internal)?;
        let mut existing = None;
        for (id, target_repo, n) in
            slath_db::merge_requests::open_by_source(&self.pool, src_id, source_ref.as_bytes())
                .await?
        {
            if target_repo == a.repo.id
                && slath_db::merge_requests::get_by_id(&self.pool, id)
                    .await?
                    .is_some_and(|m| m.target_branch == target)
            {
                existing = Some(n);
            }
        }
        let template = self.mr_template(&a, &target_head).await;
        let publishes_source = src.as_ref().is_some_and(|s| {
            crate::forks::rank(s.repo.visibility) < crate::forks::rank(a.repo.visibility)
        });
        Ok((
            a,
            Compare {
                publishes_source,
                source_repo: src.map(|s| format!("{}/{}", s.repo.namespace, s.repo.name)),
                source_branch: source,
                target_branch: target,
                source_head,
                target_head,
                merge_base,
                commits,
                more_commits,
                diff,
                diff_truncated,
                existing,
                template,
            },
        ))
    }

    /// `.slath/merge_request_template.md` as it stands on the target.
    async fn mr_template(&self, a: &RepoAccess, target_head: &ObjectId) -> Option<String> {
        let out = self
            .runner
            .repo(&self.repo_path(a.repo.id))
            .args(["cat-file", "blob"])
            .arg(format!("{}:{TEMPLATE_PATH}", target_head.as_str()))
            .output()
            .await
            .ok()?;
        let max = self.config.limits.text_max_bytes.as_usize();
        (out.status.success() && out.stdout.len() <= max)
            .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
    }

    /// Resolves user names to accounts that can read the repository.
    pub(crate) async fn readers(
        &self,
        a: &RepoAccess,
        field: &str,
        names: &[String],
    ) -> Result<Vec<UserId>> {
        let mut out = Vec::new();
        for name in names
            .iter()
            .map(|n| n.trim().trim_start_matches('@'))
            .filter(|n| !n.is_empty())
        {
            let found = match slath_db::orgs::resolve_namespace(&self.pool, name).await? {
                Some((_, _, slath_db::orgs::NamespaceKind::User(u))) => Some(u),
                _ => None,
            };
            let ok = match found {
                Some(u) => match slath_db::users::get(&self.pool, u).await? {
                    Some(user) => self
                        .repo_access_by_id(
                            &crate::notifications::user_subject(u, &user),
                            a.repo.id,
                            Action::RepoRead,
                        )
                        .await
                        .is_ok(),
                    None => false,
                },
                None => false,
            };
            match (found, ok) {
                (Some(u), true) => {
                    if !out.contains(&u) {
                        out.push(u);
                    }
                }
                _ => {
                    return Err(ServiceError::invalid(
                        field,
                        "unknown",
                        format!("{name} isn't a user who can read this repository."),
                    ));
                }
            }
        }
        Ok(out)
    }

    /// Opens a merge request from a branch of the same repository or of a
    /// repository in its fork network that the author can push to. Only
    /// one open MR is allowed per source and target; numbers are shared
    /// with issues. A source less visible than the target needs
    /// `publish_source`, which the timeline records. The first revision
    /// is recorded right away (for a fork, after fetching its branch).
    pub async fn create_mr(
        &self,
        subject: &Subject,
        ns: &str,
        repo: &str,
        m: &NewMr,
    ) -> Result<MergeRequest> {
        let a = self.mrs_access(subject, ns, repo, Action::RepoRead).await?;
        let src = self.mr_source(subject, &a, Some(&m.source_repo)).await?;
        let action = if src.is_some() {
            Action::MrCreateFork
        } else {
            Action::MrCreate
        };
        self.authorize(subject, &a, action)?;
        self.require_confirmed(subject).await?;
        let author = subject_user(subject).ok_or(ServiceError::Unauthenticated)?;
        let title = check_title(&m.title)?;
        self.check_body("description", &m.description)?;
        let source_ref = branch_ref("source_branch", &m.source_branch)?;
        let target = match m.target_branch.trim() {
            "" => a.repo.default_branch.clone(),
            t => t.to_owned(),
        };
        let target_ref = branch_ref("target_branch", &target)?;
        if src.is_none() && source_ref == target_ref {
            return Err(ServiceError::invalid(
                "source_branch",
                "same",
                "Choose a source branch other than the target.",
            ));
        }
        // The author must be able to push the source (it's their work).
        if !src.as_ref().unwrap_or(&a).can(subject, Action::RepoPush) {
            return Err(ServiceError::invalid(
                "source_branch",
                "forbidden",
                "You can only open merge requests from branches you can push to; fork the repository first.",
            ));
        }
        let src_id = src.as_ref().map_or(a.repo.id, |s| s.repo.id);
        let published = src.as_ref().filter(|s| {
            crate::forks::rank(s.repo.visibility) < crate::forks::rank(a.repo.visibility)
        });
        if published.is_some() && !m.publish_source {
            return Err(ServiceError::invalid(
                "publish_source",
                "required",
                "The source repository is less visible than this one: everyone who can read this repository will be able to read the merge request's commits. Confirm that to continue.",
            ));
        }
        if self.branch_head(src_id, &source_ref).await?.is_none() {
            return Err(ServiceError::invalid(
                "source_branch",
                "missing",
                "This branch doesn't exist.",
            ));
        }
        if self.branch_head(a.repo.id, &target_ref).await?.is_none() {
            return Err(ServiceError::invalid(
                "target_branch",
                "missing",
                "This branch doesn't exist.",
            ));
        }
        let reviewers = self.readers(&a, "reviewers", &m.reviewers).await?;
        let assignees = self.readers(&a, "assignees", &m.assignees).await?;
        let labels = if m.labels.is_empty() {
            Vec::new()
        } else {
            let found = slath_db::labels::labels_by_name(&self.pool, a.repo.id, &m.labels).await?;
            if found.len() != m.labels.len() {
                return Err(ServiceError::invalid(
                    "labels",
                    "unknown",
                    "Choose existing labels.",
                ));
            }
            found.into_iter().map(|l| l.id).collect()
        };
        let milestone = match m
            .milestone
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            None => None,
            Some(t) => Some(
                slath_db::labels::milestones(&self.pool, a.repo.id)
                    .await?
                    .into_iter()
                    .find(|x| x.title.eq_ignore_ascii_case(t))
                    .ok_or_else(|| {
                        ServiceError::invalid(
                            "milestone",
                            "unknown",
                            "Choose an existing milestone.",
                        )
                    })?
                    .id,
            ),
        };
        let delete_default = slath_db::repos::mr_settings(&self.pool, a.repo.id).await?.1;
        let mut tx = self.pool.begin().await.map_err(ServiceError::internal)?;
        let number = slath_db::issues::next_number(&mut tx, a.repo.id).await?;
        let id = slath_db::merge_requests::insert(
            &mut tx,
            &NewMergeRequest {
                repo: a.repo.id,
                number,
                author,
                title: &title,
                description: &m.description,
                draft: m.draft,
                target_branch: &target,
                source_repo: src_id,
                source_ref: source_ref.as_bytes(),
                allow_maintainer_edits: m.allow_maintainer_edits.unwrap_or(true),
                delete_source_on_merge: m.delete_source_on_merge.unwrap_or(delete_default),
                milestone,
            },
        )
        .await
        .map_err(|e| match e {
            slath_db::DbError::Conflict(_) => ServiceError::invalid(
                "source_branch",
                "exists",
                "A merge request from this branch into this target is already open.",
            ),
            e => e.into(),
        })?;
        if let Some(p) = published {
            slath_db::issues::insert_event(
                &mut *tx,
                a.repo.id,
                "merge_request",
                *id.as_uuid(),
                Some(author),
                "source_published",
                &serde_json::json!({
                    "source": format!("{}/{}", p.repo.namespace, p.repo.name),
                    "visibility": p.repo.visibility,
                }),
            )
            .await?;
        }
        for u in &assignees {
            slath_db::merge_requests::add_assignee(&mut *tx, id, *u).await?;
        }
        for l in &labels {
            slath_db::merge_requests::add_label(&mut *tx, id, *l).await?;
        }
        if !m.draft {
            for u in reviewers.iter().filter(|u| **u != author) {
                slath_db::merge_requests::add_reviewer(&mut *tx, id, *u, author).await?;
            }
        }
        tx.commit().await.map_err(ServiceError::internal)?;
        self.sync_mr(id, Some(author)).await?;
        self.audit(
            subject,
            "mr.create",
            crate::audit::Target::new("merge_request", Some(*id.as_uuid()))
                .in_repo(*a.repo.id.as_uuid()),
            serde_json::json!({"number": number, "source_repo": src.map(|s| s.repo.id.to_string())}),
        )
        .await;
        let m = slath_db::merge_requests::get_by_id(&self.pool, id)
            .await?
            .ok_or(ServiceError::NotFound)?;
        self.record_references(subject, &a, mr_from(&m), &m.description)
            .await;
        self.notify_mr(Some(author), &m, MrActivity::Opened).await;
        Ok(m)
    }

    /// The merge requests of a repository and their (open, merged, closed)
    /// counts.
    pub async fn mr_list(
        &self,
        subject: &Subject,
        ns: &str,
        repo: &str,
        filter: &crate::labels::FilterInput,
        offset: i64,
        limit: i64,
    ) -> Result<(RepoAccess, Vec<MergeRequest>, (i64, i64, i64))> {
        let a = self.mrs_access(subject, ns, repo, Action::RepoRead).await?;
        let mut f = self.resolve_filter(&a, filter).await?;
        f.state = filter
            .state
            .clone()
            .filter(|s| matches!(s.as_str(), "open" | "merged" | "closed"));
        let items =
            slath_db::merge_requests::list(&self.pool, a.repo.id, &f, offset, limit).await?;
        let counts = slath_db::merge_requests::counts(&self.pool, a.repo.id, &f).await?;
        Ok((a, items, counts))
    }

    /// One merge request.
    pub async fn mr(
        &self,
        subject: &Subject,
        ns: &str,
        repo: &str,
        number: i32,
    ) -> Result<(RepoAccess, MergeRequest)> {
        let a = self.mrs_access(subject, ns, repo, Action::RepoRead).await?;
        let m = slath_db::merge_requests::get(&self.pool, a.repo.id, number)
            .await?
            .ok_or(ServiceError::NotFound)?;
        Ok((a, m))
    }

    /// Whether merge requests are on in this repository.
    pub async fn mrs_enabled(&self, a: &RepoAccess) -> bool {
        slath_db::repos::mr_settings(&self.pool, a.repo.id)
            .await
            .is_ok_and(|s| s.0)
    }

    #[must_use]
    pub fn mr_perms(&self, subject: &Subject, a: &RepoAccess, m: &MergeRequest) -> MrPerms {
        let own = subject_user(subject).is_some_and(|u| m.author_id == Some(u));
        let triage = a.can(subject, Action::MrTriage);
        MrPerms {
            edit: triage || (own && a.can(subject, Action::MrEditOwn)),
            close: triage || (own && a.can(subject, Action::MrEditOwn)),
            comment: a.can(subject, Action::MrComment),
            triage,
            moderate: a.can(subject, Action::CommentModerate),
        }
    }

    /// Assignees, requested reviewers (with state), labels, and milestone.
    pub async fn mr_details(
        &self,
        m: &MergeRequest,
    ) -> Result<(
        Vec<String>,
        Vec<(String, String)>,
        Vec<slath_db::labels::Label>,
        Option<String>,
    )> {
        let assignees = slath_db::merge_requests::assignees(&self.pool, m.id).await?;
        let reviewers = slath_db::mr_reviews::requested(&self.pool, m.id)
            .await?
            .into_iter()
            .map(|r| (r.name, r.state))
            .collect();
        let labels = slath_db::merge_requests::labels(&self.pool, m.id).await?;
        let milestone = match m.milestone_id {
            Some(id) => slath_db::labels::milestones(&self.pool, m.repo_id)
                .await?
                .into_iter()
                .find(|x| x.id == id)
                .map(|x| x.title),
            None => None,
        };
        Ok((assignees, reviewers, labels, milestone))
    }

    /// The revisions, oldest first.
    pub async fn mr_revisions(&self, m: &MergeRequest) -> Result<Vec<Revision>> {
        Ok(slath_db::merge_requests::revisions(&self.pool, m.id).await?)
    }

    /// The commits of the current revision, oldest first.
    pub async fn mr_commits(&self, m: &MergeRequest) -> Result<Vec<LogEntry>> {
        let revs = slath_db::merge_requests::revisions(&self.pool, m.id).await?;
        let Some(r) = revs.last() else {
            return Ok(Vec::new());
        };
        let (Ok(head), base) = (
            ObjectId::parse(&r.head_oid),
            r.merge_base_oid
                .as_deref()
                .and_then(|b| ObjectId::parse(b).ok()),
        ) else {
            return Ok(Vec::new());
        };
        slath_git::browse::log_range(
            &self.runner,
            &self.repo_path(m.repo_id),
            base.as_ref(),
            &head,
            self.config.limits.review_max_commits,
        )
        .await
        .map_err(ServiceError::internal)
    }

    /// The diff of the current revision: from its merge base, or with
    /// `since`, from that earlier revision's head (what changed since).
    /// `None` without revisions or for an unknown `since`.
    pub async fn mr_diff(
        &self,
        m: &MergeRequest,
        since: Option<i32>,
        opts: slath_git::browse::DiffOptions,
    ) -> Result<Option<(Vec<u8>, bool, ObjectId)>> {
        let revs = slath_db::merge_requests::revisions(&self.pool, m.id).await?;
        let Some(r) = revs.last() else {
            return Ok(None);
        };
        let Ok(head) = ObjectId::parse(&r.head_oid) else {
            return Ok(None);
        };
        let base = match since {
            Some(seq) => {
                let Some(old) = revs.iter().find(|v| v.seq == seq && v.seq < r.seq) else {
                    return Ok(None);
                };
                ObjectId::parse(&old.head_oid).ok()
            }
            None => r
                .merge_base_oid
                .as_deref()
                .and_then(|b| ObjectId::parse(b).ok()),
        };
        slath_git::browse::diff(
            &self.runner,
            &self.repo_path(m.repo_id),
            base.as_ref(),
            &head,
            opts,
            64 * 1024 * 1024,
        )
        .await
        .map(|(d, cut)| Some((d, cut, head.clone())))
        .map_err(ServiceError::internal)
    }

    /// The conversation: comments, events, and revisions in order.
    pub async fn mr_timeline(
        &self,
        subject: &Subject,
        a: &RepoAccess,
        m: &MergeRequest,
    ) -> Result<Vec<MrTimelineItem>> {
        let me = subject_user(subject);
        let moderate = a.can(subject, Action::CommentModerate);
        let ctx = slath_markup::refs::RefContext {
            ns: a.repo.namespace.clone(),
            repo: a.repo.name.clone(),
        };
        let mut items: Vec<(time::OffsetDateTime, MrTimelineItem)> = Vec::new();
        for c in slath_db::issues::comments(&self.pool, "merge_request", *m.id.as_uuid()).await? {
            let own = me.is_some_and(|u| c.author_id == Some(u));
            let hidden = c.hidden_at.is_some();
            let show = c.deleted_at.is_none() && (!hidden || own || moderate);
            let html = if show {
                self.render_markdown(subject, Some(&ctx), &c.body).await?
            } else {
                String::new()
            };
            items.push((
                c.created_at,
                MrTimelineItem::Comment(crate::issues::TimelineItem::Comment {
                    comment: c,
                    html,
                    hidden: hidden && show,
                    own: own && a.can(subject, Action::CommentEditOwn),
                }),
            ));
        }
        for e in slath_db::issues::events(&self.pool, "merge_request", *m.id.as_uuid()).await? {
            if e.kind == "revision" {
                continue;
            }
            if e.kind == "referenced" {
                // Shown only to those who can read its source.
                if let Some(src) = self.event_source(subject, &e.data).await {
                    items.push((e.created_at, MrTimelineItem::Referenced(e, src)));
                }
                continue;
            }
            items.push((e.created_at, MrTimelineItem::Event(e)));
        }
        let mut previous = None;
        for r in slath_db::merge_requests::revisions(&self.pool, m.id).await? {
            let head = r.head_oid.clone();
            items.push((
                r.created_at,
                MrTimelineItem::Revision {
                    revision: r,
                    previous_head: previous.take(),
                },
            ));
            previous = Some(head);
        }
        for v in self.mr_reviews(m).await? {
            items.push((v.review.created_at, MrTimelineItem::Review(v)));
        }
        for t in self.mr_threads(subject, a, m).await? {
            items.push((t.thread.created_at, MrTimelineItem::Thread(t)));
        }
        items.sort_by_key(|(t, _)| *t);
        Ok(items.into_iter().map(|(_, i)| i).collect())
    }

    /// Edits title and description (author or triage).
    pub async fn edit_mr(
        &self,
        subject: &Subject,
        ns: &str,
        repo: &str,
        number: i32,
        title: &str,
        description: &str,
    ) -> Result<()> {
        let (a, m) = self.mr(subject, ns, repo, number).await?;
        if !self.mr_perms(subject, &a, &m).edit {
            return Err(ServiceError::Forbidden);
        }
        let title = check_title(title)?;
        self.check_body("description", description)?;
        let mut tx = self.pool.begin().await.map_err(ServiceError::internal)?;
        slath_db::merge_requests::update_text(&mut *tx, m.id, &title, description).await?;
        if title != m.title {
            slath_db::issues::insert_event(
                &mut *tx,
                a.repo.id,
                "merge_request",
                *m.id.as_uuid(),
                subject_user(subject),
                "renamed",
                &serde_json::json!({"from": m.title, "to": title}),
            )
            .await?;
        }
        tx.commit().await.map_err(ServiceError::internal)?;
        if description != m.description {
            self.record_references(subject, &a, mr_from(&m), description)
                .await;
        }
        Ok(())
    }

    /// Closes or reopens (author or triage). Reopening needs the source
    /// branch and no other open MR for the same source and target.
    pub async fn set_mr_open(
        &self,
        subject: &Subject,
        ns: &str,
        repo: &str,
        number: i32,
        open: bool,
    ) -> Result<()> {
        let (a, m) = self.mr(subject, ns, repo, number).await?;
        if !self.mr_perms(subject, &a, &m).close || m.state == "merged" {
            return Err(ServiceError::Forbidden);
        }
        if open {
            let source = RefName::parse(m.source_ref.clone())
                .map_err(|_| ServiceError::internal("bad source ref"))?;
            let repo_id = m.source_repo_id.ok_or_else(|| {
                ServiceError::invalid(
                    "state",
                    "no_source",
                    "The source repository was deleted; this merge request can't be reopened.",
                )
            })?;
            if self.branch_head(repo_id, &source).await?.is_none() {
                return Err(ServiceError::invalid(
                    "state",
                    "no_source",
                    "The source branch was deleted. Restore it first.",
                ));
            }
        }
        let mut tx = self.pool.begin().await.map_err(ServiceError::internal)?;
        let changed = slath_db::merge_requests::set_open(&mut *tx, m.id, open)
            .await
            .map_err(|e| match e {
                slath_db::DbError::Conflict(_) => ServiceError::invalid(
                    "state",
                    "exists",
                    "Another merge request from this branch into this target is open.",
                ),
                e => e.into(),
            })?;
        if changed {
            slath_db::issues::insert_event(
                &mut *tx,
                a.repo.id,
                "merge_request",
                *m.id.as_uuid(),
                subject_user(subject),
                if open { "reopened" } else { "closed" },
                &serde_json::json!({}),
            )
            .await?;
        }
        tx.commit().await.map_err(ServiceError::internal)?;
        if changed && open {
            self.sync_mr(m.id, subject_user(subject)).await?;
        }
        if changed {
            let what = if open {
                MrActivity::Reopened
            } else {
                MrActivity::Closed
            };
            self.notify_mr(subject_user(subject), &m, what).await;
        }
        Ok(())
    }

    /// Marks a merge request as draft or ready (author or triage).
    pub async fn set_mr_draft(
        &self,
        subject: &Subject,
        ns: &str,
        repo: &str,
        number: i32,
        draft: bool,
    ) -> Result<()> {
        let (a, m) = self.mr(subject, ns, repo, number).await?;
        if !self.mr_perms(subject, &a, &m).edit || m.state != "open" {
            return Err(ServiceError::Forbidden);
        }
        let mut tx = self.pool.begin().await.map_err(ServiceError::internal)?;
        let changed = slath_db::merge_requests::set_draft(&mut *tx, m.id, draft).await?;
        if changed {
            slath_db::issues::insert_event(
                &mut *tx,
                a.repo.id,
                "merge_request",
                *m.id.as_uuid(),
                subject_user(subject),
                if draft { "draft" } else { "ready" },
                &serde_json::json!({}),
            )
            .await?;
        }
        tx.commit().await.map_err(ServiceError::internal)?;
        if changed && !draft {
            let m = slath_db::merge_requests::get_by_id(&self.pool, m.id)
                .await?
                .ok_or(ServiceError::NotFound)?;
            self.notify_mr(subject_user(subject), &m, MrActivity::Ready)
                .await;
        }
        Ok(())
    }

    /// Adds a comment to the conversation.
    pub async fn add_mr_comment(
        &self,
        subject: &Subject,
        ns: &str,
        repo: &str,
        number: i32,
        body: &str,
    ) -> Result<slath_db::issues::Comment> {
        let (a, m) = self.mr(subject, ns, repo, number).await?;
        if !a.can(subject, Action::MrComment) {
            return Err(ServiceError::Forbidden);
        }
        self.require_confirmed(subject).await?;
        let author = subject_user(subject).ok_or(ServiceError::Unauthenticated)?;
        if body.trim().is_empty() {
            return Err(ServiceError::invalid(
                "body",
                "required",
                "Write a comment.",
            ));
        }
        self.check_body("body", body)?;
        let id = slath_db::issues::insert_comment(
            &self.pool,
            a.repo.id,
            "merge_request",
            *m.id.as_uuid(),
            author,
            body,
        )
        .await?;
        slath_db::merge_requests::touch(&self.pool, m.id).await?;
        self.record_references(subject, &a, mr_from(&m), body).await;
        self.notify_mr(Some(author), &m, MrActivity::Commented(id))
            .await;
        slath_db::issues::comment(&self.pool, id)
            .await?
            .ok_or(ServiceError::NotFound)
    }

    /// Recreates a deleted source branch from the last revision and
    /// reopens the merge request (anyone who can push the source).
    pub async fn restore_mr_branch(
        &self,
        subject: &Subject,
        ns: &str,
        repo: &str,
        number: i32,
    ) -> Result<()> {
        let (a, m) = self.mr(subject, ns, repo, number).await?;
        if !a.can(subject, Action::RepoPush)
            || m.state == "merged"
            || m.source_repo_id != Some(a.repo.id)
        {
            return Err(ServiceError::Forbidden);
        }
        let head = m
            .head_oid
            .as_deref()
            .and_then(|h| ObjectId::parse(h).ok())
            .ok_or_else(|| {
                ServiceError::invalid("branch", "no_revision", "There's no revision to restore.")
            })?;
        let source = RefName::parse(m.source_ref.clone())
            .map_err(|_| ServiceError::internal("bad source ref"))?;
        let dir = self.repo_path(a.repo.id);
        match slath_git::refs::update_cas(&self.runner, &dir, &source, Some(&head), None).await {
            Ok(()) => {}
            Err(slath_git::refs::RefUpdateError::Conflict { .. }) => {
                return Err(ServiceError::invalid(
                    "branch",
                    "exists",
                    "The branch exists again.",
                ));
            }
            Err(e) => return Err(ServiceError::internal(e)),
        }
        let zero = "0".repeat(40);
        self.record_push(
            a.repo.id,
            subject_user(subject),
            "internal",
            &[slath_git::ipc::RefUpdate {
                name: m.source_ref.clone(),
                old: zero,
                new: head.as_str().to_owned(),
            }],
        )
        .await?;
        slath_db::issues::insert_event(
            &self.pool,
            a.repo.id,
            "merge_request",
            *m.id.as_uuid(),
            subject_user(subject),
            "source_restored",
            &serde_json::json!({}),
        )
        .await?;
        if m.state == "closed" {
            self.set_mr_open(subject, ns, repo, number, true).await?;
        }
        Ok(())
    }

    /// Records a new revision when the source head differs from the current
    /// revision's head: pins it, updates `refs/merge-requests/<n>/head`, and
    /// adds the timeline entry. Returns the new revision's number.
    pub async fn sync_mr(&self, id: MergeRequestId, pusher: Option<UserId>) -> Result<Option<i32>> {
        let Some(m) = slath_db::merge_requests::get_by_id(&self.pool, id).await? else {
            return Ok(None);
        };
        let Some(source_repo) = m.source_repo_id.filter(|_| m.state == "open") else {
            return Ok(None);
        };
        let source = RefName::parse(m.source_ref.clone())
            .map_err(|_| ServiceError::internal("bad source ref"))?;
        let head = if source_repo == m.repo_id {
            self.branch_head(source_repo, &source).await?
        } else {
            self.fetch_mr_source(&m, source_repo, &source).await?
        };
        let Some(head) = head else {
            return Ok(None);
        };
        if m.head_oid.as_deref() == Some(head.as_str()) {
            return Ok(None);
        }
        let target_ref = RefName::branch(m.target_branch.as_bytes())
            .map_err(|_| ServiceError::internal("bad target"))?;
        let Some(base) = self.branch_head(m.repo_id, &target_ref).await? else {
            return Ok(None);
        };
        let dir = self.repo_path(m.repo_id);
        let merge_base = slath_git::browse::merge_base(&self.runner, &dir, &base, &head)
            .await
            .map_err(ServiceError::internal)?;
        let max = self.config.limits.review_max_commits;
        let commits =
            slath_git::browse::log_range(&self.runner, &dir, merge_base.as_ref(), &head, max + 1)
                .await
                .map_err(ServiceError::internal)?;
        let (mode, reason) = if merge_base.is_none() {
            ("whole-only", Some("unrelated-histories"))
        } else if commits.is_empty() {
            ("whole-only", Some("empty"))
        } else if commits.len() > max {
            ("whole-only", Some("too-many-commits"))
        } else if commits.iter().any(|c| c.parents.len() > 1) {
            ("whole-only", Some("merge-commits"))
        } else {
            ("per-change", None)
        };
        // Identity needs the commits only, so it's worked out before the
        // transaction.
        let plan = if mode == "per-change" {
            let oids: Vec<ObjectId> = commits.iter().map(|c| c.oid.clone()).collect();
            Some(self.plan_changes(&m, &dir, &oids).await?)
        } else {
            None
        };
        let seq = m.revisions + 1;
        let seq_u = u32::try_from(seq).map_err(ServiceError::internal)?;
        // Pin first: a dangling pin is harmless, an unpinned revision isn't.
        slath_git::refs::pin_revision(&self.runner, &dir, id, seq_u, &head)
            .await
            .map_err(ServiceError::internal)?;
        let mut tx = self.pool.begin().await.map_err(ServiceError::internal)?;
        let r = slath_db::merge_requests::insert_revision(
            &mut *tx,
            &NewRevision {
                mr: id,
                seq,
                base_oid: base.as_str(),
                merge_base_oid: merge_base.as_ref().map(ObjectId::as_str),
                head_oid: head.as_str(),
                pushed_by: pusher,
                commit_count: i32::try_from(commits.len().min(max)).unwrap_or(i32::MAX),
                mode,
                mode_reason: reason,
            },
        )
        .await;
        if let Err(slath_db::DbError::Sqlx(sqlx::Error::Database(e))) = &r
            && e.constraint() == Some("mr_revisions_mr_id_seq_key")
        {
            // Another sync recorded this revision meanwhile.
            return Ok(None);
        }
        let revision = r?;
        if let Some(plan) = plan {
            self.apply_changes(&mut tx, id, (revision, seq), pusher, plan)
                .await?;
        }
        slath_db::merge_requests::touch(&mut *tx, id).await?;
        tx.commit().await.map_err(ServiceError::internal)?;
        let number = u32::try_from(m.number).map_err(ServiceError::internal)?;
        slath_git::refs::set(&self.runner, &dir, &slath_git::refs::mr_head(number), &head)
            .await
            .map_err(ServiceError::internal)?;
        if let Err(e) = self.note_revision(id, seq).await {
            tracing::warn!(error = %e, mr = %id, "noting the approvals of a revision failed");
        }
        // The first revision comes with opening the merge request.
        if seq > 1 {
            self.notify_mr(pusher, &m, MrActivity::Revision(seq)).await;
        }
        Ok(Some(seq))
    }

    /// A fork MR's source head, fetched into the target as
    /// `refs/slath/mr/<id>/incoming` (only that commit's history comes
    /// along) once the target's push protection admits it; a head held
    /// back leaves the previously fetched one. `None` when the source
    /// branch or repository is gone, or nothing was admitted yet.
    async fn fetch_mr_source(
        &self,
        m: &MergeRequest,
        source_repo: RepoId,
        source: &RefName,
    ) -> Result<Option<ObjectId>> {
        match slath_db::repos::get(&self.pool, source_repo).await? {
            Some(r)
                if matches!(
                    r.state,
                    slath_core::authz::RepoState::Active | slath_core::authz::RepoState::Archived
                ) => {}
            _ => return Ok(None),
        }
        let Some(head) = self.branch_head(source_repo, source).await? else {
            return Ok(None);
        };
        let dir = self.repo_path(m.repo_id);
        let src_dir = self.repo_path(source_repo);
        let incoming = slath_git::refs::mr_incoming(m.id);
        let current = slath_git::refs::read(&self.runner, &dir, &incoming)
            .await
            .map_err(ServiceError::internal)?;
        if current.as_ref() == Some(&head) {
            // Back at a head that was admitted: nothing is held back.
            if m.source_findings.is_some() {
                slath_db::merge_requests::set_source_findings(&self.pool, m.id, None).await?;
            }
            return Ok(Some(head));
        }
        // The target's push protection decides before anything is copied.
        if !self.admit_fetch(m, &src_dir, &head).await? {
            return Ok(current);
        }
        slath_git::forks::fetch_commit(&self.runner, &dir, &src_dir, &head, &incoming)
            .await
            .map_err(ServiceError::internal)?;
        Ok(Some(head))
    }

    /// Follow-up of a push (`ref.updated`): a moved source creates a
    /// revision (job `mr.sync`, or `mr.fetch` when the target is another
    /// repository), a deleted one closes its merge requests.
    pub(crate) async fn mrs_after_push(
        &self,
        repo: RepoId,
        pusher: Option<UserId>,
        updates: &[slath_git::ipc::RefUpdate],
    ) -> Result<()> {
        for u in updates {
            if !u.name.starts_with(b"refs/heads/") {
                continue;
            }
            let deleted = u.new.bytes().all(|b| b == b'0');
            // Open merge requests into this branch: the ledger notes it moved.
            if !deleted
                && let Some(branch) = u
                    .name
                    .strip_prefix(b"refs/heads/")
                    .and_then(|b| std::str::from_utf8(b).ok())
            {
                for mr in slath_db::merge_requests::open_by_target(&self.pool, repo, branch).await?
                {
                    slath_db::mr_ledger::append(
                        &self.pool,
                        mr,
                        &slath_db::mr_ledger::NewEntry {
                            kind: "target_moved",
                            detail: serde_json::json!({"old": u.old, "new": u.new}),
                            actor: pusher,
                            ..Default::default()
                        },
                    )
                    .await?;
                }
            }
            for (id, target_repo, _) in
                slath_db::merge_requests::open_by_source(&self.pool, repo, &u.name).await?
            {
                if deleted {
                    let mut tx = self.pool.begin().await.map_err(ServiceError::internal)?;
                    let closed = slath_db::merge_requests::set_open(&mut *tx, id, false).await?;
                    if closed {
                        slath_db::issues::insert_event(
                            &mut *tx,
                            target_repo,
                            "merge_request",
                            *id.as_uuid(),
                            pusher,
                            "source_deleted",
                            &serde_json::json!({}),
                        )
                        .await?;
                    }
                    tx.commit().await.map_err(ServiceError::internal)?;
                    if closed
                        && let Some(m) = slath_db::merge_requests::get_by_id(&self.pool, id).await?
                    {
                        self.notify_mr(pusher, &m, MrActivity::Closed).await;
                    }
                } else {
                    let kind = if target_repo == repo {
                        MR_SYNC
                    } else {
                        MR_FETCH
                    };
                    slath_db::jobs::enqueue(
                        &self.pool,
                        &slath_db::jobs::NewJob::new(
                            "default",
                            kind,
                            serde_json::json!({
                                "mr_id": id.to_string(),
                                "pusher": pusher.map(|p| p.to_string()),
                            }),
                        )
                        .dedupe_key(format!("{kind}:{id}:{}", u.new)),
                    )
                    .await?;
                }
            }
        }
        Ok(())
    }

    /// The `ref.updated` job.
    pub(crate) async fn run_ref_updated(
        &self,
        job: &slath_db::jobs::LeasedJob,
    ) -> std::result::Result<(), slath_db::worker::JobError> {
        use slath_db::worker::JobError;
        let push: uuid::Uuid = job
            .payload
            .get("push_id")
            .and_then(|v| v.as_str())
            .and_then(|s| s.parse().ok())
            .ok_or_else(|| JobError::Permanent("payload has no valid push_id".to_owned()))?;
        let repo: RepoId = job
            .payload
            .get("repo_id")
            .and_then(|v| v.as_str())
            .and_then(|s| s.parse().ok())
            .ok_or_else(|| JobError::Permanent("payload has no valid repo_id".to_owned()))?;
        let Some((pusher, updates)) = slath_db::refs::push_updates(&self.pool, push)
            .await
            .map_err(|e| JobError::Retry(e.to_string()))?
        else {
            return Ok(());
        };
        self.mrs_after_push(repo, pusher, &updates)
            .await
            .map_err(|e| JobError::Retry(e.to_string()))?;
        self.close_after_push(repo, pusher, &updates)
            .await
            .map_err(|e| JobError::Retry(e.to_string()))
    }

    /// The `mr.sync` and `mr.fetch` jobs (`sync_mr` fetches a fork's
    /// source itself).
    pub(crate) async fn run_mr_sync(
        &self,
        job: &slath_db::jobs::LeasedJob,
    ) -> std::result::Result<(), slath_db::worker::JobError> {
        use slath_db::worker::JobError;
        let id: MergeRequestId = job
            .payload
            .get("mr_id")
            .and_then(|v| v.as_str())
            .and_then(|s| s.parse().ok())
            .ok_or_else(|| JobError::Permanent("payload has no valid mr_id".to_owned()))?;
        let pusher = job
            .payload
            .get("pusher")
            .and_then(|v| v.as_str())
            .and_then(|s| s.parse().ok());
        self.sync_mr(id, pusher)
            .await
            .map(|_| ())
            .map_err(|e| JobError::Retry(e.to_string()))
    }

    /// The `remote:` lines after a push: for each pushed branch other than
    /// the default, how the push changed its open merge requests (spec:
    /// push feedback), or a link to create one.
    pub async fn push_hints(
        &self,
        repo: RepoId,
        pusher: Option<UserId>,
        updates: &[slath_git::ipc::RefUpdate],
    ) -> Vec<String> {
        let Ok(Some(r)) = slath_db::repos::get(&self.pool, repo).await else {
            return Vec::new();
        };
        if !slath_db::repos::mr_settings(&self.pool, repo)
            .await
            .is_ok_and(|s| s.0)
        {
            return Vec::new();
        }
        let parent = self.fork_mr_target(repo, pusher).await;
        let mut out = Vec::new();
        for u in updates {
            let Some(branch) = u.name.strip_prefix(b"refs/heads/") else {
                continue;
            };
            if u.new.bytes().all(|b| b == b'0') || branch == r.default_branch.as_bytes() {
                continue;
            }
            let Ok(branch) = std::str::from_utf8(branch) else {
                continue;
            };
            let base = self
                .config
                .server
                .public_url
                .join(&format!("/{}/{}", r.namespace, r.name));
            match slath_db::merge_requests::open_by_source(&self.pool, repo, &u.name).await {
                Ok(open) if !open.is_empty() => {
                    for (id, target, n) in open {
                        // A fork's merge requests live in their target.
                        let (url, label) = if target == repo {
                            (format!("{base}/merge-requests/{n}"), format!("!{n}"))
                        } else {
                            match slath_db::repos::get(&self.pool, target).await {
                                Ok(Some(t)) => (
                                    self.config.server.public_url.join(&format!(
                                        "/{}/{}/merge-requests/{n}",
                                        t.namespace, t.name
                                    )),
                                    format!("{}/{}!{n}", t.namespace, t.name),
                                ),
                                _ => continue,
                            }
                        };
                        out.extend(
                            self.push_feedback(
                                id,
                                pusher,
                                &u.new,
                                &format!("{label} for {branch}"),
                                &url,
                            )
                            .await,
                        );
                    }
                }
                Ok(_) => {
                    // From a fork, the merge request goes to the parent.
                    let (target, mut q) = match &parent {
                        Some(p) => {
                            let mut q = url::form_urlencoded::Serializer::new(String::new());
                            q.append_pair("source_repo", &format!("{}/{}", r.namespace, r.name));
                            (self.config.server.public_url.join(&format!("/{p}")), q)
                        }
                        None => (
                            base.clone(),
                            url::form_urlencoded::Serializer::new(String::new()),
                        ),
                    };
                    let q = q.append_pair("source", branch).finish();
                    out.push(format!("slath: Create a merge request for {branch}:"));
                    out.push(format!("slath:   {target}/compare?{q}"));
                }
                Err(e) => {
                    tracing::warn!(error = %e, "looking up merge requests for push hints failed")
                }
            }
        }
        out
    }

    /// `ns/name` of a fork's parent that `user` can open merge requests in
    /// from it, if any.
    async fn fork_mr_target(&self, fork: RepoId, user: Option<UserId>) -> Option<String> {
        let u = user?;
        let user = slath_db::users::get(&self.pool, u).await.ok()??;
        let s = crate::notifications::user_subject(u, &user);
        let a = self
            .repo_access_by_id(&s, fork, Action::RepoRead)
            .await
            .ok()?;
        let f = self.fork_status(&s, &a).await.ok()??;
        f.contribute.then_some(f.parent).flatten()
    }

    /// The (at most three) branches the viewer pushed most recently in the
    /// last hour that have no open merge request (the "Recently pushed"
    /// banner).
    pub async fn recently_pushed(&self, subject: &Subject, a: &RepoAccess) -> Result<Vec<String>> {
        let Some(me) = subject_user(subject) else {
            return Ok(Vec::new());
        };
        if !a.can(subject, Action::MrCreate)
            || !slath_db::repos::mr_settings(&self.pool, a.repo.id).await?.0
        {
            return Ok(Vec::new());
        }
        let since = time::OffsetDateTime::now_utc() - time::Duration::hours(1);
        let mut out = Vec::new();
        for name in slath_db::refs::recent_branch_pushes(&self.pool, a.repo.id, me, since).await? {
            let Ok(r) = RefName::parse(name.clone()) else {
                continue;
            };
            let Some(branch) = name
                .strip_prefix(b"refs/heads/")
                .and_then(|b| std::str::from_utf8(b).ok())
            else {
                continue;
            };
            if branch == a.repo.default_branch || self.branch_head(a.repo.id, &r).await?.is_none() {
                continue;
            }
            if slath_db::merge_requests::open_by_source(&self.pool, a.repo.id, &name)
                .await?
                .is_empty()
            {
                out.push(branch.to_owned());
                if out.len() == 3 {
                    break;
                }
            }
        }
        Ok(out)
    }
}
