// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Issues: creation with shared numbers, editing, closing, comments, the
//! timeline, and Markdown with references resolved for each viewer.
//!
//! Permissions follow the matrix (spec/04): any signed-in reader opens
//! issues and comments; authors edit and close their own; `issue.triage`
//! (write) edits and closes any; `comment.moderate` (maintain) hides and
//! deletes others' comments. A reference to another issue records a
//! `referenced` event there if the actor can read it, and that event is
//! shown only to viewers who can read where it came from.

use std::collections::HashMap;

use slath_core::authz::{self, Action, Subject};
use slath_core::ids::{RepoId, UserId};
use slath_db::issues::{Comment, Filter, Issue};
use slath_markup::refs::{Ref, RefContext, Resolved};

use crate::Services;
use crate::error::{Result, ServiceError};
use crate::repos::RepoAccess;

/// Longest title in characters.
const MAX_TITLE: usize = 256;
/// Commit references checked against Git per document.
const MAX_COMMIT_REFS: usize = 20;

/// What a viewer may do with an issue.
#[derive(Debug, Clone, Copy, Default)]
pub struct IssuePerms {
    pub edit: bool,
    pub close: bool,
    pub comment: bool,
    pub triage: bool,
    pub moderate: bool,
}

/// One entry of an issue's timeline as a viewer sees it.
#[derive(Debug, Clone)]
pub enum TimelineItem {
    Comment {
        comment: Comment,
        /// Rendered body (empty when deleted or hidden from this viewer).
        html: String,
        /// Hidden by a moderator and the viewer may still see it.
        hidden: bool,
        /// The viewer may edit or delete it.
        own: bool,
    },
    Event(slath_db::issues::Event, Option<String>),
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
    if t.chars().count() > MAX_TITLE || t.chars().any(|c| c.is_control()) {
        return Err(ServiceError::invalid(
            "title",
            "invalid",
            format!("Use at most {MAX_TITLE} characters, without control characters."),
        ));
    }
    Ok(t.to_owned())
}

impl Services {
    fn check_body(&self, field: &str, body: &str) -> Result<()> {
        let max = self.config.limits.text_max_bytes.as_usize();
        if body.len() > max {
            return Err(ServiceError::invalid(
                field,
                "too_long",
                format!("This text is too long (at most {} KiB).", max / 1024),
            ));
        }
        Ok(())
    }

    /// The repository with `action` allowed, if issues are enabled there.
    async fn issues_access(
        &self,
        subject: &Subject,
        ns: &str,
        repo: &str,
        action: Action,
    ) -> Result<RepoAccess> {
        let a = self.repo_access(subject, ns, repo, action).await?;
        if !slath_db::repos::issues_enabled(&self.pool, a.repo.id).await? {
            return Err(ServiceError::NotFound);
        }
        Ok(a)
    }

    /// The issues of a repository and its `(open, closed)` counts.
    pub async fn issue_list(
        &self,
        subject: &Subject,
        ns: &str,
        repo: &str,
        filter: &Filter,
        offset: i64,
        limit: i64,
    ) -> Result<(RepoAccess, Vec<Issue>, (i64, i64))> {
        let a = self
            .issues_access(subject, ns, repo, Action::RepoRead)
            .await?;
        let items = slath_db::issues::list(&self.pool, a.repo.id, filter, offset, limit).await?;
        let counts = slath_db::issues::counts(&self.pool, a.repo.id).await?;
        Ok((a, items, counts))
    }

    /// Opens an issue. Numbers are unique per repository (shared with merge
    /// requests) even under concurrent creation.
    pub async fn create_issue(
        &self,
        subject: &Subject,
        ns: &str,
        repo: &str,
        title: &str,
        body: &str,
    ) -> Result<Issue> {
        let a = self
            .issues_access(subject, ns, repo, Action::IssueCreate)
            .await?;
        self.require_confirmed(subject).await?;
        let author = subject_user(subject).ok_or(ServiceError::Unauthenticated)?;
        let title = check_title(title)?;
        self.check_body("body", body)?;
        let mut tx = self.pool.begin().await.map_err(ServiceError::internal)?;
        let number = slath_db::issues::next_number(&mut tx, a.repo.id).await?;
        slath_db::issues::insert(&mut tx, a.repo.id, number, author, &title, body).await?;
        tx.commit().await.map_err(ServiceError::internal)?;
        let issue = slath_db::issues::get(&self.pool, a.repo.id, number)
            .await?
            .ok_or(ServiceError::NotFound)?;
        self.record_references(subject, &a, &issue, body).await;
        self.issue_created(subject, &a, &issue).await;
        Ok(issue)
    }

    /// Hook for notifications (filled in by the notifications module).
    async fn issue_created(&self, subject: &Subject, a: &RepoAccess, issue: &Issue) {
        self.notify_item(subject, a, issue, crate::notifications::Activity::Opened)
            .await;
    }

    /// One issue.
    pub async fn issue(
        &self,
        subject: &Subject,
        ns: &str,
        repo: &str,
        number: i32,
    ) -> Result<(RepoAccess, Issue)> {
        let a = self
            .issues_access(subject, ns, repo, Action::RepoRead)
            .await?;
        let issue = slath_db::issues::get(&self.pool, a.repo.id, number)
            .await?
            .ok_or(ServiceError::NotFound)?;
        Ok((a, issue))
    }

    /// What the subject may do with an issue.
    #[must_use]
    pub fn issue_perms(&self, subject: &Subject, a: &RepoAccess, issue: &Issue) -> IssuePerms {
        let own = subject_user(subject).is_some_and(|u| issue.author_id == Some(u));
        let triage = a.can(subject, Action::IssueTriage);
        IssuePerms {
            edit: triage || (own && a.can(subject, Action::IssueEditOwn)),
            close: triage || (own && a.can(subject, Action::IssueEditOwn)),
            comment: a.can(subject, Action::CommentCreate),
            triage,
            moderate: a.can(subject, Action::CommentModerate),
        }
    }

    /// The timeline: comments and events in order, as the viewer sees them.
    pub async fn issue_timeline(
        &self,
        subject: &Subject,
        a: &RepoAccess,
        issue: &Issue,
    ) -> Result<Vec<TimelineItem>> {
        let me = subject_user(subject);
        let moderate = a.can(subject, Action::CommentModerate);
        let ctx = RefContext {
            ns: a.repo.namespace.clone(),
            repo: a.repo.name.clone(),
        };
        let mut items = Vec::new();
        for c in slath_db::issues::comments(&self.pool, "issue", issue.id).await? {
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
                TimelineItem::Comment {
                    comment: c,
                    html,
                    hidden: hidden && show,
                    own: own && a.can(subject, Action::CommentEditOwn),
                },
            ));
        }
        for e in slath_db::issues::events(&self.pool, "issue", issue.id).await? {
            // A reference is shown only to those who can read its source.
            let mut source = None;
            if e.kind == "referenced" {
                let repo = e
                    .data
                    .get("repo_id")
                    .and_then(|v| v.as_str())
                    .and_then(|s| s.parse().ok());
                let Some(repo_id) = repo else { continue };
                let Ok(src) = self
                    .repo_access_by_id(subject, RepoId::from_uuid(repo_id), Action::RepoRead)
                    .await
                else {
                    continue;
                };
                let n = e
                    .data
                    .get("number")
                    .and_then(serde_json::Value::as_i64)
                    .unwrap_or(0);
                source = Some(format!("{}/{}#{n}", src.repo.namespace, src.repo.name));
            }
            items.push((e.created_at, TimelineItem::Event(e, source)));
        }
        items.sort_by_key(|(t, _)| *t);
        Ok(items.into_iter().map(|(_, i)| i).collect())
    }

    /// Edits title and body (author or triage). A changed title is recorded.
    pub async fn edit_issue(
        &self,
        subject: &Subject,
        ns: &str,
        repo: &str,
        number: i32,
        title: &str,
        body: &str,
    ) -> Result<()> {
        let (a, issue) = self.issue(subject, ns, repo, number).await?;
        if !self.issue_perms(subject, &a, &issue).edit {
            return Err(ServiceError::Forbidden);
        }
        let title = check_title(title)?;
        self.check_body("body", body)?;
        let mut tx = self.pool.begin().await.map_err(ServiceError::internal)?;
        slath_db::issues::update_text(&mut *tx, issue.id, &title, body).await?;
        if title != issue.title {
            slath_db::issues::insert_event(
                &mut *tx,
                a.repo.id,
                "issue",
                issue.id,
                subject_user(subject),
                "renamed",
                &serde_json::json!({"from": issue.title, "to": title}),
            )
            .await?;
        }
        tx.commit().await.map_err(ServiceError::internal)?;
        if body != issue.body {
            self.record_references(subject, &a, &issue, body).await;
        }
        Ok(())
    }

    /// Closes (with a reason) or reopens (author or triage).
    pub async fn set_issue_state(
        &self,
        subject: &Subject,
        ns: &str,
        repo: &str,
        number: i32,
        open: bool,
        reason: Option<&str>,
    ) -> Result<()> {
        let (a, issue) = self.issue(subject, ns, repo, number).await?;
        if !self.issue_perms(subject, &a, &issue).close {
            return Err(ServiceError::Forbidden);
        }
        let reason = if open {
            None
        } else {
            match reason.unwrap_or("completed") {
                r @ ("completed" | "not_planned" | "duplicate") => Some(r),
                _ => {
                    return Err(ServiceError::invalid(
                        "reason",
                        "invalid",
                        "Choose a reason.",
                    ));
                }
            }
        };
        let mut tx = self.pool.begin().await.map_err(ServiceError::internal)?;
        if slath_db::issues::set_state(&mut *tx, issue.id, open, reason).await? {
            slath_db::issues::insert_event(
                &mut *tx,
                a.repo.id,
                "issue",
                issue.id,
                subject_user(subject),
                if open { "reopened" } else { "closed" },
                &serde_json::json!({"reason": reason}),
            )
            .await?;
        }
        tx.commit().await.map_err(ServiceError::internal)?;
        self.notify_item(
            subject,
            &a,
            &issue,
            if open {
                crate::notifications::Activity::Reopened
            } else {
                crate::notifications::Activity::Closed
            },
        )
        .await;
        Ok(())
    }

    /// Adds a comment.
    pub async fn add_issue_comment(
        &self,
        subject: &Subject,
        ns: &str,
        repo: &str,
        number: i32,
        body: &str,
    ) -> Result<Comment> {
        let (a, issue) = self.issue(subject, ns, repo, number).await?;
        if !a.can(subject, Action::CommentCreate) {
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
            &self.pool, a.repo.id, "issue", issue.id, author, body,
        )
        .await?;
        slath_db::issues::touch(&self.pool, issue.id).await?;
        self.record_references(subject, &a, &issue, body).await;
        self.notify_item(
            subject,
            &a,
            &issue,
            crate::notifications::Activity::Commented(id),
        )
        .await;
        slath_db::issues::comment(&self.pool, id)
            .await?
            .ok_or(ServiceError::NotFound)
    }

    /// A comment of an issue in the repository, with the access.
    async fn issue_comment(
        &self,
        subject: &Subject,
        ns: &str,
        repo: &str,
        id: uuid::Uuid,
    ) -> Result<(RepoAccess, Comment)> {
        let a = self
            .issues_access(subject, ns, repo, Action::RepoRead)
            .await?;
        let c = slath_db::issues::comment(&self.pool, id)
            .await?
            .filter(|c| c.repo_id == a.repo.id && c.deleted_at.is_none())
            .ok_or(ServiceError::NotFound)?;
        Ok((a, c))
    }

    /// Edits one's own comment.
    pub async fn edit_issue_comment(
        &self,
        subject: &Subject,
        ns: &str,
        repo: &str,
        id: uuid::Uuid,
        body: &str,
    ) -> Result<()> {
        let (a, c) = self.issue_comment(subject, ns, repo, id).await?;
        let own = subject_user(subject).is_some_and(|u| c.author_id == Some(u));
        if !own || !a.can(subject, Action::CommentEditOwn) {
            return Err(ServiceError::Forbidden);
        }
        if body.trim().is_empty() {
            return Err(ServiceError::invalid(
                "body",
                "required",
                "Write a comment.",
            ));
        }
        self.check_body("body", body)?;
        slath_db::issues::edit_comment(&self.pool, id, body).await?;
        Ok(())
    }

    /// Deletes a comment (own, or any with `comment.moderate`).
    pub async fn delete_issue_comment(
        &self,
        subject: &Subject,
        ns: &str,
        repo: &str,
        id: uuid::Uuid,
    ) -> Result<()> {
        let (a, c) = self.issue_comment(subject, ns, repo, id).await?;
        let own = subject_user(subject).is_some_and(|u| c.author_id == Some(u))
            && a.can(subject, Action::CommentEditOwn);
        if !own && !a.can(subject, Action::CommentModerate) {
            return Err(ServiceError::Forbidden);
        }
        slath_db::issues::delete_comment(&self.pool, id).await?;
        if !own {
            self.audit(
                subject,
                "comment.delete",
                crate::audit::Target::repo(*a.repo.id.as_uuid()),
                serde_json::json!({"comment": id}),
            )
            .await;
        }
        Ok(())
    }

    /// Hides or shows a comment (moderators).
    pub async fn hide_issue_comment(
        &self,
        subject: &Subject,
        ns: &str,
        repo: &str,
        id: uuid::Uuid,
        hidden: bool,
    ) -> Result<()> {
        let (a, _) = self.issue_comment(subject, ns, repo, id).await?;
        if !a.can(subject, Action::CommentModerate) {
            return Err(ServiceError::Forbidden);
        }
        slath_db::issues::set_comment_hidden(&self.pool, id, hidden).await?;
        self.audit(
            subject,
            if hidden {
                "comment.hide"
            } else {
                "comment.unhide"
            },
            crate::audit::Target::repo(*a.repo.id.as_uuid()),
            serde_json::json!({"comment": id}),
        )
        .await;
        Ok(())
    }

    /// Records `referenced` events in the issues a text refers to (other
    /// than `issue` itself) that the actor can read.
    async fn record_references(
        &self,
        subject: &Subject,
        a: &RepoAccess,
        issue: &Issue,
        body: &str,
    ) {
        let ctx = RefContext {
            ns: a.repo.namespace.clone(),
            repo: a.repo.name.clone(),
        };
        let html = slath_markup::render_with_refs(
            body,
            slath_markup::MarkdownOptions::default(),
            Some(&ctx),
        );
        for r in slath_markup::refs::references(&html) {
            let Ref::Item { ns, repo, number } = r else {
                continue;
            };
            let Ok(number) = i32::try_from(number) else {
                continue;
            };
            let Ok(target) = self
                .issues_access(subject, &ns, &repo, Action::RepoRead)
                .await
            else {
                continue;
            };
            if target.repo.id == a.repo.id && number == issue.number {
                continue;
            }
            let Ok(Some(t)) = slath_db::issues::get(&self.pool, target.repo.id, number).await
            else {
                continue;
            };
            let _ = slath_db::issues::insert_event(
                &self.pool,
                target.repo.id,
                "issue",
                t.id,
                subject_user(subject),
                "referenced",
                &serde_json::json!({"repo_id": a.repo.id.to_string(), "number": issue.number}),
            )
            .await;
        }
    }

    /// Markdown for one viewer: the cached render with references resolved
    /// against what the viewer can read. Inaccessible references become
    /// plain text, revealing neither existence nor title.
    pub async fn render_markdown(
        &self,
        subject: &Subject,
        ctx: Option<&RefContext>,
        src: &str,
    ) -> Result<String> {
        let opts = slath_markup::MarkdownOptions {
            external_images: self.config.ui.external_images,
        };
        let html = slath_markup::render_with_refs(src, opts, ctx);
        let refs = slath_markup::refs::references(&html);
        if refs.is_empty() {
            return Ok(html.to_string());
        }
        let mut resolved: HashMap<Ref, Resolved> = HashMap::new();
        let mut repos: HashMap<(String, String), Option<RepoAccess>> = HashMap::new();
        let mut commits = 0;
        for r in &refs {
            let target = match r {
                Ref::Mention { name } => {
                    let exists = matches!(
                        slath_db::orgs::resolve_namespace(&self.pool, name).await?,
                        Some((_, _, slath_db::orgs::NamespaceKind::User(_)))
                    );
                    let res = if exists {
                        Resolved::Link {
                            href: format!("/{name}"),
                            title: None,
                            state: None,
                        }
                    } else {
                        Resolved::Text
                    };
                    resolved.insert(r.clone(), res);
                    continue;
                }
                Ref::Item { ns, repo, .. } | Ref::Commit { ns, repo, .. } => {
                    (ns.clone(), repo.clone())
                }
            };
            if !repos.contains_key(&target) {
                let a = self
                    .repo_access(subject, &target.0, &target.1, Action::RepoRead)
                    .await
                    .ok();
                repos.insert(target.clone(), a);
            }
            let Some(a) = repos.get(&target).and_then(Option::as_ref) else {
                resolved.insert(r.clone(), Resolved::Text);
                continue;
            };
            let base = format!("/{}/{}", a.repo.namespace, a.repo.name);
            let res = match r {
                Ref::Item { number, .. } => {
                    let n = i32::try_from(*number).unwrap_or(0);
                    let enabled = slath_db::repos::issues_enabled(&self.pool, a.repo.id).await?;
                    match slath_db::issues::titles(&self.pool, a.repo.id, &[n])
                        .await?
                        .pop()
                    {
                        Some((_, title, state)) if enabled => Resolved::Link {
                            href: format!("{base}/issues/{n}"),
                            title: Some(format!("{title} ({state})")),
                            state: Some(if state == "open" { "open" } else { "closed" }),
                        },
                        _ => Resolved::Text,
                    }
                }
                Ref::Commit { sha, .. } => {
                    commits += 1;
                    if commits > MAX_COMMIT_REFS {
                        Resolved::Text
                    } else {
                        match slath_git::storage::resolve_commit_prefix(
                            &self.runner,
                            &self.repo_path(a.repo.id),
                            sha,
                        )
                        .await
                        {
                            Ok(Some(full)) => Resolved::Link {
                                href: format!("{base}/commit/{full}"),
                                title: None,
                                state: None,
                            },
                            _ => Resolved::Text,
                        }
                    }
                }
                Ref::Mention { .. } => Resolved::Text,
            };
            resolved.insert(r.clone(), res);
        }
        Ok(slath_markup::refs::resolve(&html, ctx, |r| {
            resolved.get(r).cloned().unwrap_or(Resolved::Text)
        }))
    }

    /// Whether `a` allows `action` for the subject (templates).
    #[must_use]
    pub fn can(&self, subject: &Subject, a: &RepoAccess, action: Action) -> bool {
        authz::check(subject, action, &a.ctx).is_allowed()
    }
}
