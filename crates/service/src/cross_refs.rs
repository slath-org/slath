// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! References between issues and merge requests.
//!
//! A `#n` or `ns/repo#n` in an issue or merge request (its text or a
//! comment) records a `referenced` event in what it points at, if the actor
//! can read it. The event names its source, and it's shown only to viewers
//! who can read the source, so a private repository's references reveal
//! nothing to the readers of a public one.
//!
//! Closing keywords (`Fixes #n`, `Closes ns/repo#n`) in commit messages
//! that land on the default branch, and in the description of a merge
//! request merged into it, close the issue they name if the pusher or
//! merger may close it there. Anything they can't read stays untouched.

use slath_core::authz::{Action, Subject};
use slath_core::ids::{RepoId, UserId};
use slath_markup::refs::{Ref, RefContext};

use crate::Services;
use crate::repos::RepoAccess;

/// Where a text with references was written.
#[derive(Debug, Clone, Copy)]
pub(crate) struct RefFrom {
    /// `issue` or `merge_request`.
    pub kind: &'static str,
    pub number: i32,
}

/// What a timeline event points back at, for a viewer who can read it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefSource {
    /// `ns/name` of the repository.
    pub repo: String,
    pub target: RefTarget,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RefTarget {
    Issue(i32),
    MergeRequest(i32),
    Commit(String),
}

impl RefSource {
    /// The page of the source.
    #[must_use]
    pub fn path(&self) -> String {
        match &self.target {
            RefTarget::Issue(n) => format!("/{}/issues/{n}", self.repo),
            RefTarget::MergeRequest(n) => format!("/{}/merge-requests/{n}", self.repo),
            RefTarget::Commit(oid) => format!("/{}/commit/{oid}", self.repo),
        }
    }

    /// How the source is written: `ns/repo#n` or `ns/repo@abc1234`.
    #[must_use]
    pub fn label(&self) -> String {
        match &self.target {
            RefTarget::Issue(n) | RefTarget::MergeRequest(n) => format!("{}#{n}", self.repo),
            RefTarget::Commit(oid) => {
                format!("{}@{}", self.repo, oid.get(..12).unwrap_or(oid))
            }
        }
    }
}

fn subject_user(subject: &Subject) -> Option<UserId> {
    match subject {
        Subject::User { id, .. } => Some(*id),
        _ => None,
    }
}

impl Services {
    /// The issue or merge request a reference points at, if the subject can
    /// read it: `(repo, kind, id, number)`.
    pub(crate) async fn referenced_item(
        &self,
        subject: &Subject,
        r: &Ref,
    ) -> Option<(RepoAccess, &'static str, uuid::Uuid, i32)> {
        let Ref::Item { ns, repo, number } = r else {
            return None;
        };
        let number = i32::try_from(*number).ok()?;
        let a = self
            .repo_access(subject, ns, repo, Action::RepoRead)
            .await
            .ok()?;
        if slath_db::repos::issues_enabled(&self.pool, a.repo.id)
            .await
            .ok()?
            && let Ok(Some(i)) = slath_db::issues::get(&self.pool, a.repo.id, number).await
        {
            return Some((a, "issue", i.id, number));
        }
        if self.mrs_enabled(&a).await
            && let Ok(Some(m)) = slath_db::merge_requests::get(&self.pool, a.repo.id, number).await
        {
            return Some((a, "merge_request", *m.id.as_uuid(), number));
        }
        None
    }

    /// Records a `referenced` event in every issue and merge request `body`
    /// points at (other than its source) that the actor can read, once per
    /// source.
    pub(crate) async fn record_references(
        &self,
        subject: &Subject,
        a: &RepoAccess,
        from: RefFrom,
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
            let Some((target, kind, id, number)) = self.referenced_item(subject, &r).await else {
                continue;
            };
            if target.repo.id == a.repo.id && number == from.number {
                continue;
            }
            let data = serde_json::json!({
                "repo_id": a.repo.id.to_string(),
                "number": from.number,
                "kind": from.kind,
            });
            if slath_db::issues::has_event(&self.pool, kind, id, "referenced", &data)
                .await
                .unwrap_or(true)
            {
                continue;
            }
            let _ = slath_db::issues::insert_event(
                &self.pool,
                target.repo.id,
                kind,
                id,
                subject_user(subject),
                "referenced",
                &data,
            )
            .await;
        }
    }

    /// What an event points back at (`repo_id` with `number` and `kind`, or
    /// with `commit`, in `data`), if the viewer can read that repository.
    pub(crate) async fn event_source(
        &self,
        subject: &Subject,
        data: &serde_json::Value,
    ) -> Option<RefSource> {
        let repo = data
            .get("repo_id")
            .and_then(|v| v.as_str())
            .and_then(|s| s.parse().ok())?;
        let src = self
            .repo_access_by_id(subject, RepoId::from_uuid(repo), Action::RepoRead)
            .await
            .ok()?;
        let target = if let Some(oid) = data.get("commit").and_then(|v| v.as_str()) {
            RefTarget::Commit(oid.to_owned())
        } else {
            let n = i32::try_from(data.get("number")?.as_i64()?).ok()?;
            match data.get("kind").and_then(|v| v.as_str()) {
                Some("merge_request") => RefTarget::MergeRequest(n),
                _ => RefTarget::Issue(n),
            }
        };
        Some(RefSource {
            repo: format!("{}/{}", src.repo.namespace, src.repo.name),
            target,
        })
    }

    /// Closes the issues `text` names with closing keywords that the actor
    /// may close, recording `via` (what closed them) in the event.
    async fn close_by_keywords(
        &self,
        actor: &Subject,
        a: &RepoAccess,
        text: &str,
        via: &serde_json::Value,
    ) -> crate::error::Result<()> {
        let ctx = RefContext {
            ns: a.repo.namespace.clone(),
            repo: a.repo.name.clone(),
        };
        for r in slath_markup::refs::closing_refs(text, &ctx) {
            let Some((target, "issue", _, number)) = self.referenced_item(actor, &r).await else {
                continue;
            };
            let Some(issue) = slath_db::issues::get(&self.pool, target.repo.id, number).await?
            else {
                continue;
            };
            if issue.state != "open" || !self.issue_perms(actor, &target, &issue).close {
                continue;
            }
            let mut tx = self
                .pool
                .begin()
                .await
                .map_err(crate::error::ServiceError::internal)?;
            let closed =
                slath_db::issues::set_state(&mut *tx, issue.id, false, Some("completed")).await?;
            if closed {
                slath_db::issues::insert_event(
                    &mut *tx,
                    target.repo.id,
                    "issue",
                    issue.id,
                    subject_user(actor),
                    "closed",
                    &serde_json::json!({"reason": "completed", "via": via}),
                )
                .await?;
            }
            tx.commit()
                .await
                .map_err(crate::error::ServiceError::internal)?;
            if closed {
                self.notify_item(
                    actor,
                    &target,
                    &issue,
                    crate::notifications::Activity::Closed,
                )
                .await;
            }
        }
        Ok(())
    }

    /// Follow-up of a push (`ref.updated`): closing keywords in what landed
    /// on the default branch, from merged merge requests' descriptions and
    /// from commit messages (at most `review_max_commits` per update).
    pub(crate) async fn close_after_push(
        &self,
        repo: RepoId,
        pusher: Option<UserId>,
        updates: &[slath_git::ipc::RefUpdate],
    ) -> crate::error::Result<()> {
        let Some(user) = pusher else {
            return Ok(());
        };
        let Some(row) = slath_db::users::get(&self.pool, user).await? else {
            return Ok(());
        };
        let actor = crate::notifications::user_subject(user, &row);
        let Ok(a) = self.repo_access_by_id(&actor, repo, Action::RepoRead).await else {
            return Ok(());
        };
        let default = format!("refs/heads/{}", a.repo.default_branch);
        let repo_id = a.repo.id.to_string();
        for u in updates {
            if u.name != default.as_bytes() || u.new.bytes().all(|b| b == b'0') {
                continue;
            }
            let new = slath_git::ObjectId::parse(u.new.as_bytes())
                .map_err(crate::error::ServiceError::internal)?;
            let old = if u.old.bytes().all(|b| b == b'0') {
                None
            } else {
                Some(
                    slath_git::ObjectId::parse(u.old.as_bytes())
                        .map_err(crate::error::ServiceError::internal)?,
                )
            };
            for id in slath_db::merge_requests::merged_as(&self.pool, repo, &u.new).await? {
                let Some(m) = slath_db::merge_requests::get_by_id(&self.pool, id).await? else {
                    continue;
                };
                if m.target_branch == a.repo.default_branch {
                    let via = serde_json::json!({
                        "repo_id": repo_id,
                        "number": m.number,
                        "kind": "merge_request",
                    });
                    self.close_by_keywords(&actor, &a, &m.description, &via)
                        .await?;
                }
            }
            let commits = slath_git::browse::messages_range(
                &self.runner,
                &self.repo_path(repo),
                old.as_ref(),
                &new,
                self.config.limits.review_max_commits,
            )
            .await
            .map_err(crate::error::ServiceError::internal)?;
            for (oid, message) in commits {
                let via = serde_json::json!({"repo_id": repo_id, "commit": oid.as_str()});
                self.close_by_keywords(&actor, &a, &message, &via).await?;
            }
        }
        Ok(())
    }
}
