// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! References between issues and merge requests.
//!
//! A `#n` or `ns/repo#n` in an issue or merge request (its text or a
//! comment) records a `referenced` event in what it points at, if the actor
//! can read it. The event names its source, and it's shown only to viewers
//! who can read the source, so a private repository's references reveal
//! nothing to the readers of a public one.

use slath_core::authz::{Action, Subject};
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

fn subject_user(subject: &Subject) -> Option<slath_core::ids::UserId> {
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
            .repo_access_by_id(
                subject,
                slath_core::ids::RepoId::from_uuid(repo),
                Action::RepoRead,
            )
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
}
