// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Fork merge requests as branches (D-44 item 4): the target repository
//! shows each open fork merge request as the branch `mr/<n>` at its current
//! head, so Git and jj can check it out, and push edits to it, without
//! adding the fork as a remote. The server writes these branches; nobody
//! else may create branches under `mr/`.
//!
//! They're hidden from fetches except for the people involved: requested
//! reviewers, assignees, and whoever chose "Check out", and for pushes the
//! people who may edit the merge request as maintainers. That's fetch
//! hygiene, not confidentiality (D-50): everything in the target is
//! readable by everyone who can read it.

use slath_core::authz::{Action, Subject};
use slath_core::ids::RepoId;
use slath_db::merge_requests::MergeRequest;

use crate::Services;
use crate::error::{Result, ServiceError};

impl Services {
    /// The `mr/<n>` branches a transport request shows to the subject
    /// (`write`: receive-pack).
    pub async fn shown_mr_branches(
        &self,
        subject: &Subject,
        repo: RepoId,
        write: bool,
    ) -> Vec<u32> {
        let Subject::User { id, .. } = subject else {
            return Vec::new();
        };
        let mut shown = slath_db::merge_requests::involved_fork_mrs(&self.pool, repo, *id)
            .await
            .unwrap_or_default();
        if write
            && let Ok(a) = self
                .repo_access_by_id(subject, repo, Action::RepoPushMaintainerEdit)
                .await
        {
            // Maintainer edits go through `mr/<n>`: such pushers see them.
            if let Ok(editable) =
                slath_db::merge_requests::maintainer_editable_into(&self.pool, a.repo.id).await
            {
                shown.extend(editable);
            }
        }
        shown.sort_unstable();
        shown.dedup();
        shown
            .into_iter()
            .filter_map(|n| u32::try_from(n).ok())
            .collect()
    }

    /// Puts the branch `mr/<n>` of a fork merge request where it belongs:
    /// at its head while it's open, gone otherwise. Same-repository merge
    /// requests have none. Failures only cost the branch until next time.
    pub(crate) async fn sync_mr_branch(&self, m: &MergeRequest) {
        if m.source_repo_id.is_none_or(|s| s == m.repo_id) {
            return;
        }
        let Ok(number) = u32::try_from(m.number) else {
            return;
        };
        let dir = self.repo_path(m.repo_id);
        let name = slath_git::refs::mr_branch(number);
        let current = match slath_git::refs::read(&self.runner, &dir, &name).await {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(error = %e, mr = %m.id, "reading a merge request branch failed");
                return;
            }
        };
        let want = if m.state == "open" {
            m.head_oid
                .as_deref()
                .and_then(|h| slath_git::ObjectId::parse(h).ok())
        } else {
            None
        };
        if current == want {
            return;
        }
        let r = match &want {
            Some(head) => slath_git::refs::set(&self.runner, &dir, &name, head)
                .await
                .map_err(ServiceError::internal),
            None => slath_git::refs::update_cas(&self.runner, &dir, &name, None, current.as_ref())
                .await
                .map_err(ServiceError::internal),
        };
        if let Err(e) = r {
            tracing::warn!(error = %e, mr = %m.id, "updating a merge request branch failed");
        }
    }

    /// [`Self::sync_mr_branch`] by ID, after its state changed.
    pub(crate) async fn sync_mr_branch_by_id(&self, id: slath_core::ids::MergeRequestId) {
        if let Ok(Some(m)) = slath_db::merge_requests::get_by_id(&self.pool, id).await {
            self.sync_mr_branch(&m).await;
        }
    }

    /// Records that the subject chose "Check out": the merge request's
    /// branch shows in their fetches from now on.
    pub async fn note_checkout(&self, subject: &Subject, m: &MergeRequest) -> Result<()> {
        if let Subject::User { id, .. } = subject {
            slath_db::merge_requests::add_checkout(&self.pool, m.id, *id).await?;
        }
        Ok(())
    }
}
