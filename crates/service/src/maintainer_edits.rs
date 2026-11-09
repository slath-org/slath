// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Edits by maintainers: writers of a merge request's target may push to
//! its source branch in a fork while the merge request is open and allows
//! it. The target must be upstream of the fork (a merge request from an
//! upstream repository into a fork grants nothing), the author must still
//! be able to push to the fork, and the right covers updates of that one
//! branch only: no deletion, no other branches, no tags.

use slath_core::authz::{Action, Subject};
use slath_core::ids::{MergeRequestId, RepoId};

use crate::Services;
use crate::error::{Result, ServiceError};

impl Services {
    /// The branches of `fork` (full ref names) that `subject` may update
    /// as a maintainer, each with the merge request that allows it.
    pub(crate) async fn maintainer_edit_grants(
        &self,
        subject: &Subject,
        fork: RepoId,
    ) -> Result<Vec<(Vec<u8>, MergeRequestId)>> {
        let mut out = Vec::new();
        for m in slath_db::merge_requests::maintainer_editable(&self.pool, fork).await? {
            if !slath_db::forks::is_upstream(&self.pool, m.target, fork).await? {
                continue;
            }
            match self
                .repo_access_by_id(subject, m.target, Action::RepoPushMaintainerEdit)
                .await
            {
                Ok(_) => {}
                Err(ServiceError::NotFound | ServiceError::Forbidden) => continue,
                Err(e) => return Err(e),
            }
            if !self.author_can_push(m.author, fork).await? {
                continue;
            }
            out.push((m.source_ref, m.id));
        }
        Ok(out)
    }

    /// Whether a merge request's author can (still) push to `repo`.
    async fn author_can_push(
        &self,
        author: Option<slath_core::ids::UserId>,
        repo: RepoId,
    ) -> Result<bool> {
        let Some(u) = author else {
            return Ok(false);
        };
        let Some(user) = slath_db::users::get(&self.pool, u).await? else {
            return Ok(false);
        };
        let s = crate::notifications::user_subject(u, &user);
        match self.repo_access_by_id(&s, repo, Action::RepoPush).await {
            Ok(_) => Ok(true),
            Err(ServiceError::NotFound | ServiceError::Forbidden) => Ok(false),
            Err(e) => Err(e),
        }
    }

    /// Turns edits by maintainers on or off for a fork merge request. Only
    /// someone who can push to the source may change it; the timeline
    /// records it.
    pub async fn set_maintainer_edits(
        &self,
        subject: &Subject,
        ns: &str,
        repo: &str,
        number: i32,
        allow: bool,
    ) -> Result<()> {
        let (a, m) = self.mr(subject, ns, repo, number).await?;
        let source = m
            .source_repo_id
            .filter(|s| *s != a.repo.id)
            .ok_or_else(|| {
                ServiceError::invalid(
                    "allow_maintainer_edits",
                    "not_fork",
                    "Only merge requests from forks have this setting.",
                )
            })?;
        match self
            .repo_access_by_id(subject, source, Action::RepoPush)
            .await
        {
            Ok(_) => {}
            Err(ServiceError::NotFound | ServiceError::Forbidden) => {
                return Err(ServiceError::Forbidden);
            }
            Err(e) => return Err(e),
        }
        let mut tx = self.pool.begin().await.map_err(ServiceError::internal)?;
        if slath_db::merge_requests::set_allow_maintainer_edits(&mut *tx, m.id, allow).await? {
            let user = match subject {
                Subject::User { id, .. } => Some(*id),
                _ => None,
            };
            slath_db::issues::insert_event(
                &mut *tx,
                a.repo.id,
                "merge_request",
                *m.id.as_uuid(),
                user,
                if allow {
                    "maintainer_edits_allowed"
                } else {
                    "maintainer_edits_disallowed"
                },
                &serde_json::json!({}),
            )
            .await?;
        }
        tx.commit().await.map_err(ServiceError::internal)?;
        Ok(())
    }
}
