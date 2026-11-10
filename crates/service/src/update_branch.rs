// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Updating a merge request's branch with its target on the server: rebase
//! it onto the target (with the merge engine, so headers, authors, and
//! change IDs survive) or merge the target into it. Whoever may push to the
//! source branch may do it, and so may target writers on a fork merge
//! request that allows edits by maintainers. The source's branch protection
//! applies as it would to a push, and the branch moves only by
//! compare-and-swap. The new head becomes a revision right away.

use slath_core::authz::{Action, RepoState, Subject};
use slath_db::merge_requests::MergeRequest;
use slath_git::ObjectId;
use slath_git::args::RefName;
use slath_git::merge::{self, TreeMerge};
use slath_git::refs::RefUpdateError;

use crate::Services;
use crate::error::{Result, ServiceError};

fn internal(e: impl std::fmt::Display) -> ServiceError {
    ServiceError::internal(e.to_string())
}

impl Services {
    /// Whether the subject may update the merge request's source branch:
    /// `Some((access to the source, by edits of maintainers))`, or `None`.
    async fn branch_update_right(
        &self,
        subject: &Subject,
        a: &crate::repos::RepoAccess,
        m: &MergeRequest,
    ) -> Result<Option<(Option<crate::repos::RepoAccess>, bool)>> {
        let Some(src) = m.source_repo_id else {
            return Ok(None);
        };
        if !matches!(subject, Subject::User { .. }) {
            return Ok(None);
        }
        let src_access = match self.repo_access_by_id(subject, src, Action::RepoRead).await {
            Ok(s) => Some(s),
            Err(ServiceError::NotFound | ServiceError::Forbidden) => None,
            Err(e) => return Err(e),
        };
        if src_access
            .as_ref()
            .is_some_and(|s| s.can(subject, Action::RepoPush))
        {
            return Ok(Some((src_access, false)));
        }
        let edit = src != a.repo.id
            && self
                .maintainer_edit_grants(subject, src)
                .await?
                .iter()
                .any(|(r, id)| *id == m.id && *r == m.source_ref);
        Ok(edit.then_some((src_access, true)))
    }

    /// Whether the subject may update the branch (the merge box offers it).
    pub async fn may_update_branch(
        &self,
        subject: &Subject,
        a: &crate::repos::RepoAccess,
        m: &MergeRequest,
    ) -> bool {
        m.state == "open"
            && self
                .branch_update_right(subject, a, m)
                .await
                .is_ok_and(|r| r.is_some())
    }

    /// Brings the source branch up to date with the target, by `rebase` or
    /// `merge`. `expected_head_oid`, if given, must be the current head.
    pub async fn update_mr_branch(
        &self,
        subject: &Subject,
        (ns, repo, number): (&str, &str, i32),
        method: &str,
        expected_head_oid: Option<&str>,
    ) -> Result<MergeRequest> {
        let (a, m) = self.mr(subject, ns, repo, number).await?;
        let Subject::User { id: user, .. } = subject else {
            return Err(ServiceError::Unauthenticated);
        };
        if !matches!(method, "rebase" | "merge") {
            return Err(ServiceError::invalid(
                "method",
                "invalid",
                "Choose rebase or merge.",
            ));
        }
        if m.state != "open" {
            return Err(ServiceError::Conflict(
                "The merge request isn't open.".into(),
            ));
        }
        let head = m.head_oid.as_deref().ok_or_else(|| {
            ServiceError::Conflict("The merge request has no revision yet.".into())
        })?;
        if expected_head_oid.is_some_and(|e| e != head) {
            return Err(ServiceError::Conflict(format!(
                "The merge request changed since you looked: it's at revision {} now.",
                m.revisions
            )));
        }
        let head = ObjectId::parse(head).map_err(internal)?;
        let src = m
            .source_repo_id
            .ok_or_else(|| ServiceError::Conflict("The source repository is gone.".into()))?;
        let Some((src_access, maintainer_edit)) = self.branch_update_right(subject, &a, &m).await?
        else {
            return Err(ServiceError::Forbidden);
        };
        if src_access
            .as_ref()
            .is_some_and(|s| s.repo.state == RepoState::Archived)
        {
            return Err(ServiceError::Conflict(
                "The source repository is archived.".into(),
            ));
        }
        let src_dir = self.repo_path(src);
        let dir = self.repo_path(a.repo.id);
        let source = RefName::parse(m.source_ref.clone()).map_err(|_| internal("bad source"))?;
        if slath_git::refs::read(&self.runner, &src_dir, &source)
            .await
            .map_err(internal)?
            .as_ref()
            != Some(&head)
        {
            return Err(ServiceError::Conflict(
                "The source branch moved since the last revision; wait for it to show up and try again."
                    .into(),
            ));
        }
        let target =
            RefName::branch(m.target_branch.as_bytes()).map_err(|_| internal("bad target"))?;
        let t = slath_git::refs::read(&self.runner, &dir, &target)
            .await
            .map_err(internal)?
            .ok_or_else(|| ServiceError::Conflict("The target branch doesn't exist.".into()))?;
        if merge::is_ancestor(&self.runner, &dir, &t, &head)
            .await
            .map_err(internal)?
        {
            return Err(ServiceError::Conflict(format!(
                "The branch is already up to date with {}.",
                m.target_branch
            )));
        }
        // The source's branch protection, as for a push by this user.
        let branch = &m.source_ref[b"refs/heads/".len().min(m.source_ref.len())..];
        let rules = slath_db::protections::list(&self.pool, src).await?;
        let p = crate::protections::effective(&rules, branch);
        if p.protected {
            let role = src_access.as_ref().and_then(|s| s.ctx.role);
            let refused = if method == "rebase" && p.block_force_push {
                Some(
                    "its branch protection doesn't allow force pushes, which a rebase is; merge instead",
                )
            } else if p.nobody || p.require_mr {
                Some("its branch protection only allows changes through merge requests")
            } else if p.push_role.is_some_and(|min| role.is_none_or(|r| r < min)) {
                Some("its branch protection needs a higher role to push")
            } else {
                None
            };
            if let Some(why) = refused {
                return Err(ServiceError::Conflict(format!(
                    "The branch can't be updated: {why}."
                )));
            }
        }
        let committer = self.identity(subject).await?;
        let new = if method == "rebase" {
            let ids = self.change_ids(&m).await?;
            let mut signatures = Vec::new();
            self.build(
                &a,
                &m,
                ("rebase", &t, &head),
                &committer,
                &ids,
                None,
                &mut signatures,
            )
            .await?
        } else {
            let tree = match merge::merge_trees(&self.runner, &dir, None, &head, &t)
                .await
                .map_err(internal)?
            {
                TreeMerge::Clean(tree) => tree,
                TreeMerge::Conflicts(files) => {
                    return Err(ServiceError::Conflict(format!(
                        "Merging {} into the branch conflicts in: {}.",
                        m.target_branch,
                        files.join(", ")
                    )));
                }
            };
            let text = format!(
                "Merge branch '{}' into {}",
                m.target_branch,
                m.source_branch()
            );
            let c = merge::new_commit(
                &tree,
                &[head.clone(), t.clone()],
                &committer,
                &committer,
                None,
                &text,
            );
            merge::write_commit(&self.runner, &dir, &c)
                .await
                .map_err(internal)?
        };
        // A fork needs the new commits before its branch can point at them.
        let tmp = (src != a.repo.id).then(slath_git::refs::sync_temp);
        if let Some(tmp) = &tmp {
            slath_git::forks::fetch_commit(&self.runner, &src_dir, &dir, &new, tmp)
                .await
                .map_err(internal)?;
        }
        let moved =
            slath_git::refs::update_cas(&self.runner, &src_dir, &source, Some(&new), Some(&head))
                .await;
        if let Some(tmp) = &tmp {
            let _ =
                slath_git::refs::update_cas(&self.runner, &src_dir, tmp, None, Some(&new)).await;
        }
        match moved {
            Ok(()) => {}
            Err(RefUpdateError::Conflict { .. }) => {
                return Err(ServiceError::Conflict(
                    "The source branch moved meanwhile; try again.".into(),
                ));
            }
            Err(e) => return Err(internal(e)),
        }
        slath_db::issues::insert_event(
            &self.pool,
            a.repo.id,
            "merge_request",
            *m.id.as_uuid(),
            Some(*user),
            "branch_updated",
            &serde_json::json!({"method": method, "target": m.target_branch, "from": head.as_str(), "to": new.as_str()}),
        )
        .await?;
        self.record_push(
            src,
            Some(*user),
            "internal",
            &[slath_git::ipc::RefUpdate {
                name: m.source_ref.clone(),
                old: head.as_str().to_owned(),
                new: new.as_str().to_owned(),
            }],
        )
        .await?;
        self.sync_mr(m.id, Some(*user)).await?;
        self.audit(
            subject,
            if maintainer_edit {
                "push.maintainer_edit"
            } else {
                "mr.update_branch"
            },
            crate::audit::Target::new("merge_request", Some(*m.id.as_uuid()))
                .in_repo(*a.repo.id.as_uuid()),
            serde_json::json!({"number": m.number, "method": method, "oid": new.as_str()}),
        )
        .await;
        slath_db::merge_requests::get_by_id(&self.pool, m.id)
            .await?
            .ok_or(ServiceError::NotFound)
    }
}
