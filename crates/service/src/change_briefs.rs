// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Change briefs: what the author of a change version says about it (what
//! it's for, how it does it, how it was checked, and what's missing),
//! shown next to the code as its author's claim.
//!
//! A brief belongs to a change and one of its versions' commits, so a
//! revision that keeps a change's commit keeps its brief. Writing again
//! adds an edit; a version's brief is its newest edit. Everyone who can
//! read the merge request reads its briefs; its author and those who may
//! push its source branch write them, while it's open. Every text is
//! scanned for secrets before anything is stored. The schema is in
//! [`crate::brief_schema`].

use slath_core::authz::{Subject, UserKind};
use slath_db::change_briefs::{Brief, NewBrief};
use slath_db::merge_requests::MergeRequest;

use crate::Services;
use crate::brief_schema::{Item, Upload, stored_items};
use crate::error::{Result, ServiceError};
use crate::mr_changes::ChangeInfo;
use crate::repos::RepoAccess;

pub use crate::brief_schema::SCHEMA;

/// A brief as its readers see it.
#[derive(Debug, Clone)]
pub struct ChangeBrief {
    pub brief: Brief,
    pub items: Vec<Item>,
    pub author: Option<String>,
    pub author_is_bot: bool,
    /// The number of the change's first version with this commit.
    pub version: Option<usize>,
}

impl Services {
    /// The change `id` names (display ID, a prefix of at least 8
    /// characters, or a version's commit) with its row ID.
    async fn brief_change(&self, m: &MergeRequest, id: &str) -> Result<(uuid::Uuid, ChangeInfo)> {
        let info = self.mr_change(m, id).await?;
        let row = slath_db::mr_changes::changes(&self.pool, m.id)
            .await?
            .into_iter()
            .find(|c| c.display_id == info.display_id)
            .ok_or(ServiceError::NotFound)?;
        Ok((row.id, info))
    }

    /// Whether `subject` may write briefs of `m` now: it's open, and they
    /// are its author or may push to the repository (the rule for
    /// rebinding its commits, so token scopes and archiving apply).
    #[must_use]
    pub fn may_write_brief(subject: &Subject, a: &RepoAccess, m: &MergeRequest) -> bool {
        m.state == "open" && Self::can_rebind(subject, a, m)
    }

    /// Writes a brief (`body` is its JSON) for a version of a change.
    pub async fn write_change_brief(
        &self,
        subject: &Subject,
        (ns, repo, number): (&str, &str, i32),
        change: &str,
        body: &[u8],
    ) -> Result<ChangeBrief> {
        let (a, m) = self.mr(subject, ns, repo, number).await?;
        let (change_id, info) = self.brief_change(&m, change).await?;
        let Subject::User { id: author, .. } = subject else {
            return Err(ServiceError::Unauthenticated);
        };
        if !Self::can_rebind(subject, &a, &m) {
            return Err(ServiceError::Forbidden);
        }
        if m.state != "open" {
            return Err(ServiceError::Conflict(
                "This merge request isn't open, so its briefs can't change.".into(),
            ));
        }
        let limit = self.config.limits.brief_max_bytes.0;
        if body.len() as u64 > limit {
            return Err(ServiceError::invalid(
                "body",
                "too_large",
                format!("Send a brief of at most {} KiB.", limit / 1024),
            ));
        }
        let mut upload = Upload::parse(body)?;
        let commit = match &upload.commit {
            None => info
                .versions
                .last()
                .map(|v| v.commit_oid.clone())
                .ok_or(ServiceError::NotFound)?,
            Some(c) => info
                .versions
                .iter()
                .find(|v| &v.commit_oid == c)
                .map(|v| v.commit_oid.clone())
                .ok_or_else(|| {
                    ServiceError::invalid(
                        "/commit",
                        "commit",
                        "Name the commit of one of this change's versions.",
                    )
                })?,
        };
        // Anchors point into this version's own diff (parent to commit).
        if upload.items.iter().any(|i| !i.anchors.is_empty()) {
            let v = info
                .versions
                .iter()
                .find(|v| v.commit_oid == commit)
                .ok_or(ServiceError::NotFound)?;
            let parse = |s: &str| slath_git::ObjectId::parse(s).map_err(ServiceError::internal);
            let base = v.parent_oid.as_deref().map(parse).transpose()?;
            let (diff, _, _) = self
                .parsed_diff(a.repo.id, base.as_ref(), &parse(&commit)?)
                .await?;
            upload.locate(&diff)?;
        }
        self.refuse_secrets(upload.texts())?;
        let items = serde_json::to_value(&upload.items).map_err(ServiceError::internal)?;
        let origin = self.origin_of(subject, &a.repo).await?;
        let brief = slath_db::change_briefs::insert(
            &self.pool,
            &NewBrief {
                mr_id: m.id,
                change_id,
                commit_oid: &commit,
                author_id: *author,
                origin: origin.as_str(),
                items: &items,
            },
        )
        .await?;
        self.brief_view(&info, brief).await
    }

    async fn brief_view(&self, info: &ChangeInfo, brief: Brief) -> Result<ChangeBrief> {
        let author = match brief.author_id {
            Some(id) => slath_db::users::get(&self.pool, id).await?,
            None => None,
        };
        Ok(ChangeBrief {
            version: info
                .versions
                .iter()
                .find(|v| v.commit_oid == brief.commit_oid)
                .map(|v| v.number),
            author_is_bot: author.as_ref().is_some_and(|u| u.kind == UserKind::Bot),
            author: author.map(|u| u.name),
            items: stored_items(&brief.items),
            brief,
        })
    }

    /// A change's briefs, newest first, for everyone who can read the
    /// merge request; with `commit`, only that version's.
    pub async fn change_briefs(
        &self,
        subject: &Subject,
        (ns, repo, number): (&str, &str, i32),
        change: &str,
        commit: Option<&str>,
        want: usize,
    ) -> Result<Vec<ChangeBrief>> {
        let (_, m) = self.mr(subject, ns, repo, number).await?;
        self.mr_change_briefs(&m, change, commit, want).await
    }

    /// The same for a merge request the caller may read.
    pub async fn mr_change_briefs(
        &self,
        m: &MergeRequest,
        change: &str,
        commit: Option<&str>,
        want: usize,
    ) -> Result<Vec<ChangeBrief>> {
        let (change_id, info) = self.brief_change(m, change).await?;
        let rows = slath_db::change_briefs::list(
            &self.pool,
            change_id,
            commit,
            i64::try_from(want).unwrap_or(i64::MAX),
        )
        .await?;
        let mut out = Vec::with_capacity(rows.len());
        for b in rows {
            out.push(self.brief_view(&info, b).await?);
        }
        Ok(out)
    }
}
