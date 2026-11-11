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
//!
//! A push to `mr/<n>` is an edit by a maintainer: allowed while the merge
//! request is open and allows them, for whoever may make them, if the
//! fork's branch is still where the pusher saw it. After the push the
//! server moves the fork's branch by compare-and-swap; if the fork moved
//! meanwhile, the push output says so and `mr/<n>` goes back.

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

    /// Pre-receive for a push to `mr/<n>` in `repo`: `Ok` with the audit
    /// record if the subject may make this maintainer edit, else why not.
    /// `force`: the new head doesn't contain the old one.
    pub(crate) async fn mr_branch_push_check(
        &self,
        subject: &Subject,
        repo: RepoId,
        u: &slath_git::ipc::RefUpdate,
        force: bool,
    ) -> std::result::Result<serde_json::Value, String> {
        const RESERVED: &str =
            "branches under mr/ show merge requests from forks; Slath writes them";
        let Some(n) = u
            .name
            .strip_prefix(slath_git::refs::MR_BRANCH_PREFIX)
            .and_then(|n| std::str::from_utf8(n).ok())
            .filter(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
            .and_then(|n| n.parse::<i32>().ok())
        else {
            return Err(RESERVED.to_owned());
        };
        if u.new.bytes().all(|b| b == b'0') {
            return Err(format!(
                "mr/{n} can't be deleted; close the merge request instead"
            ));
        }
        let m = match slath_db::merge_requests::get(&self.pool, repo, n).await {
            Ok(Some(m)) if m.state == "open" && m.source_repo_id.is_some_and(|s| s != repo) => m,
            _ => return Err(RESERVED.to_owned()),
        };
        let src = m.source_repo_id.unwrap_or(repo);
        let allowed = self
            .maintainer_edit_grants(subject, src)
            .await
            .map_err(|_| "the push couldn't be checked; try again".to_owned())?
            .iter()
            .any(|(r, id)| *id == m.id && *r == m.source_ref);
        if !allowed {
            return Err(format!(
                "only maintainers of this repository can push to mr/{n}, and only while its merge request allows edits by maintainers"
            ));
        }
        let source = slath_git::args::RefName::parse(m.source_ref.clone())
            .map_err(|_| "the merge request's source is invalid".to_owned())?;
        let head = slath_git::refs::read(&self.runner, &self.repo_path(src), &source)
            .await
            .map_err(|_| "the push couldn't be checked; try again".to_owned())?;
        if head.as_ref().map(slath_git::ObjectId::as_str) != Some(u.old.as_str()) {
            return Err(format!(
                "mr/{n} is behind the fork's branch; fetch and try again"
            ));
        }
        let w = crate::update_branch::SourceWrite {
            src,
            role: None,
            maintainer_edit: true,
            source,
            head: slath_git::ObjectId::parse(u.old.as_bytes())
                .map_err(|_| "the push couldn't be checked; try again".to_owned())?,
        };
        if let Some(why) = self
            .source_protection_refusal(&w, force)
            .await
            .map_err(|_| "the push couldn't be checked; try again".to_owned())?
        {
            return Err(format!("the fork's branch can't take it: {why}"));
        }
        Ok(serde_json::json!({
            "ref": String::from_utf8_lossy(&u.name),
            "merge_request": m.id.to_string(),
        }))
    }

    /// Post-receive for a push to `mr/<n>`: moves the fork's branch to the
    /// pushed head by compare-and-swap. Returns the line for the push
    /// output.
    pub(crate) async fn propagate_mr_branch(
        &self,
        subject: &Subject,
        repo: RepoId,
        u: &slath_git::ipc::RefUpdate,
    ) -> Option<String> {
        let n: i32 = std::str::from_utf8(u.name.strip_prefix(slath_git::refs::MR_BRANCH_PREFIX)?)
            .ok()?
            .parse()
            .ok()?;
        let Subject::User { id: user, .. } = subject else {
            return None;
        };
        let m = slath_db::merge_requests::get(&self.pool, repo, n)
            .await
            .ok()??;
        let src = m.source_repo_id?;
        let a = self
            .repo_access_by_id(subject, repo, Action::RepoRead)
            .await
            .ok()?;
        let source = slath_git::args::RefName::parse(m.source_ref.clone()).ok()?;
        let w = crate::update_branch::SourceWrite {
            src,
            role: None,
            maintainer_edit: true,
            source,
            head: slath_git::ObjectId::parse(u.old.as_bytes()).ok()?,
        };
        let new = slath_git::ObjectId::parse(u.new.as_bytes()).ok()?;
        match self.move_source(&w, &a, &m, &new, *user).await {
            Ok(()) => {
                let fork = slath_db::repos::get(&self.pool, src)
                    .await
                    .ok()
                    .flatten()
                    .map(|r| format!("{}/{}", r.namespace, r.name))
                    .unwrap_or_default();
                Some(format!(
                    "slath: mr/{n}: updated {fork}:{}",
                    m.source_branch()
                ))
            }
            Err(e) => {
                tracing::info!(error = %e, mr = %m.id, "a push to an mr/ branch didn't reach the fork");
                if let Ok(Some(m)) = slath_db::merge_requests::get_by_id(&self.pool, m.id).await {
                    self.sync_mr_branch(&m).await;
                }
                Some(format!(
                    "slath: mr/{n}: the fork's branch moved meanwhile, so this push didn't reach it; fetch and try again"
                ))
            }
        }
    }
}
