// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Partial landing: merging the bottom changes of a stack (up to one
//! change) while the merge request stays open with the rest. The prefix
//! lands with the same strategies and requirements as a whole merge,
//! counted for its changes. Afterwards the rest is a new revision based on
//! the new target: after a rebase or squash, Slath rebases the rest onto
//! the landed commits and moves the source branch by compare-and-swap
//! (change IDs carry over, so identities continue); after a fast-forward
//! or merge commit, the rest already builds on what landed and keeps its
//! commits.

use std::collections::HashMap;

use slath_core::authz::Subject;
use slath_core::ids::UserId;
use slath_db::merge_requests::MergeRequest;
use slath_git::ObjectId;
use slath_git::args::RefName;
use slath_git::refs::RefUpdateError;

use crate::Services;
use crate::error::{Result, ServiceError};
use crate::merge::MergeInput;
use crate::repos::RepoAccess;
use crate::update_branch::SourceWrite;

/// How often landing is rebuilt when the target moved meanwhile.
const ATTEMPTS: usize = 3;

/// The bottom changes of the current revision that partial landing merges.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Prefix {
    /// How many changes, counted from the bottom: at least one, fewer than
    /// the stack has.
    pub len: usize,
    /// The commit of the prefix's top change.
    pub head: String,
    /// The display IDs of its changes, bottom first.
    pub changes: Vec<String>,
}

fn internal(e: impl std::fmt::Display) -> ServiceError {
    ServiceError::internal(e.to_string())
}

fn oid(s: &str) -> Result<ObjectId> {
    ObjectId::parse(s).map_err(internal)
}

impl Services {
    /// The prefix up to `change` (its display ID, or at least 8 of its
    /// first characters) in the current revision; `None` when it's the top
    /// change, which lands the whole merge request.
    pub async fn mr_prefix(&self, m: &MergeRequest, change: &str) -> Result<Option<Prefix>> {
        let unknown = || {
            ServiceError::invalid(
                "upto_change",
                "unknown",
                format!("The current revision has no change {change}."),
            )
        };
        let stack = self.mr_stack(m).await?;
        let ids: Vec<&str> = stack
            .entries
            .iter()
            .map(|e| e.display_id.as_deref().unwrap_or(""))
            .collect();
        let found: Vec<usize> = ids
            .iter()
            .enumerate()
            .filter(|(_, d)| {
                !d.is_empty() && (**d == change || (change.len() >= 8 && d.starts_with(change)))
            })
            .map(|(i, _)| i)
            .collect();
        let [i] = found.as_slice() else {
            return Err(unknown());
        };
        if *i + 1 == stack.entries.len() {
            return Ok(None);
        }
        Ok(Some(Prefix {
            len: i + 1,
            head: stack.entries[*i].commit_oid.clone(),
            changes: ids[..=*i].iter().map(|d| (*d).to_owned()).collect(),
        }))
    }

    /// Lands `p` with `strategy` (already allowed, requirements met), then
    /// makes the rest a new revision. A rebase or squash rewrites the
    /// rest, so it needs the right to write the source branch, checked
    /// before anything moves.
    pub(crate) async fn land_prefix(
        &self,
        subject: &Subject,
        (a, m): (&RepoAccess, &MergeRequest),
        p: &Prefix,
        strategy: &str,
        input: &MergeInput,
        user: UserId,
    ) -> Result<MergeRequest> {
        let head = oid(&input.expected_head_oid)?;
        let top = oid(&p.head)?;
        let rewrites = matches!(strategy, "rebase" | "squash");
        let w = if rewrites {
            Some(self.rest_write(subject, a, m, &head).await?)
        } else {
            None
        };
        let committer = self.identity(subject).await?;
        let dir = self.repo_path(a.repo.id);
        let target =
            RefName::branch(m.target_branch.as_bytes()).map_err(|_| internal("bad target"))?;
        let ids = self.change_ids(m).await?;
        let mut tx = self.pool.begin().await.map_err(internal)?;
        slath_db::merge_requests::lock_target(&mut *tx, a.repo.id, &m.target_branch).await?;
        match slath_db::merge_requests::get_by_id(&mut *tx, m.id).await? {
            Some(now) if now.state == "open" && now.head_oid == m.head_oid => {}
            _ => {
                return Err(ServiceError::Conflict(
                    "The merge request changed while merging; reload it.".into(),
                ));
            }
        }
        let mut landed = None;
        let mut signatures: Vec<(String, String)> = Vec::new();
        for _ in 0..ATTEMPTS {
            let t = slath_git::refs::read(&self.runner, &dir, &target)
                .await
                .map_err(internal)?
                .ok_or(ServiceError::Conflict(
                    "The target branch doesn't exist.".into(),
                ))?;
            signatures.clear();
            let (result, rest) = self
                .build_prefix(
                    (a, m),
                    p,
                    (strategy, &t, &top, &head),
                    &committer,
                    &ids,
                    input.commit_message.as_deref(),
                    &mut signatures,
                )
                .await?;
            match slath_git::refs::update_cas(&self.runner, &dir, &target, Some(&result), Some(&t))
                .await
            {
                Ok(()) => {
                    landed = Some((t, result, rest));
                    break;
                }
                Err(RefUpdateError::Conflict { .. }) => {}
                Err(e) => return Err(internal(e)),
            }
        }
        let Some((old, result, rest)) = landed else {
            return Err(ServiceError::Conflict(
                "The target branch kept moving while merging; try again.".into(),
            ));
        };
        let changes = slath_db::mr_changes::changes(&self.pool, m.id).await?;
        let merged: Vec<uuid::Uuid> = changes
            .iter()
            .filter(|c| p.changes.contains(&c.display_id))
            .map(|c| c.id)
            .collect();
        slath_db::mr_changes::set_changes_merged(&mut *tx, &merged).await?;
        for (old_c, new_c) in &signatures {
            slath_db::mr_ledger::append(
                &mut *tx,
                m.id,
                &slath_db::mr_ledger::NewEntry {
                    revision_seq: Some(m.revisions),
                    kind: "signature_removed",
                    change_id: ids.get(old_c).map(String::as_str),
                    reason: Some("rebase"),
                    detail: serde_json::json!({"commit": old_c, "new_commit": new_c}),
                    actor: Some(user),
                    ..Default::default()
                },
            )
            .await?;
        }
        slath_db::issues::insert_event(
            &mut *tx,
            a.repo.id,
            "merge_request",
            *m.id.as_uuid(),
            Some(user),
            "changes_landed",
            &serde_json::json!({
                "changes": p.changes,
                "oid": result.as_str(),
                "target": m.target_branch,
                "strategy": strategy,
            }),
        )
        .await?;
        tx.commit().await.map_err(internal)?;
        self.record_push(
            a.repo.id,
            Some(user),
            "internal",
            &[slath_git::ipc::RefUpdate {
                name: target.as_bytes().to_vec(),
                old: old.as_str().to_owned(),
                new: result.as_str().to_owned(),
            }],
        )
        .await?;
        // The rest becomes a new revision on the new target.
        match (w, rest) {
            (Some(w), Some(rest)) => {
                // Someone pushed the source meanwhile: their push is the
                // next revision, and a later rebase skips what landed.
                if let Err(e) = self.move_source(&w, a, m, &rest, user).await {
                    tracing::info!(error = %e, mr = %m.id, "the rest of a partially landed stack wasn't rebased");
                }
            }
            _ => {
                self.resync_mr(m.id, Some(user)).await?;
            }
        }
        self.audit(
            subject,
            "mr.merge",
            crate::audit::Target::new("merge_request", Some(*m.id.as_uuid()))
                .in_repo(*a.repo.id.as_uuid()),
            serde_json::json!({
                "number": m.number,
                "strategy": strategy,
                "oid": result.as_str(),
                "upto_change": p.changes.last(),
            }),
        )
        .await;
        slath_db::merge_requests::get_by_id(&self.pool, m.id)
            .await?
            .ok_or(ServiceError::NotFound)
    }

    /// The right to rewrite the rest of the stack on the source branch,
    /// as for updating the branch with a rebase; refused in words that say
    /// which strategies still work.
    async fn rest_write(
        &self,
        subject: &Subject,
        a: &RepoAccess,
        m: &MergeRequest,
        head: &ObjectId,
    ) -> Result<SourceWrite> {
        let other = "land it with a merge commit or fast-forward, or merge it whole";
        let w = match self.source_write(subject, a, m, head).await {
            Ok(w) => w,
            Err(ServiceError::Forbidden) => {
                return Err(ServiceError::Conflict(format!(
                    "Landing part of it by rebase or squash rebases the rest of the branch, which you can't push to; {other}."
                )));
            }
            Err(e) => return Err(e),
        };
        if let Some(why) = self.source_protection_refusal(subject, &w, true).await? {
            return Err(ServiceError::Conflict(format!(
                "Landing part of it by rebase or squash rebases the rest of the branch, but {why}; {other}."
            )));
        }
        Ok(w)
    }

    /// What the target moves to for the prefix, and the rest of the stack
    /// rebased onto it when the strategy rewrites the prefix.
    #[allow(clippy::too_many_arguments)]
    async fn build_prefix(
        &self,
        (a, m): (&RepoAccess, &MergeRequest),
        p: &Prefix,
        (strategy, t, top, head): (&str, &ObjectId, &ObjectId, &ObjectId),
        committer: &str,
        ids: &HashMap<String, String>,
        message: Option<&str>,
        signatures: &mut Vec<(String, String)>,
    ) -> Result<(ObjectId, Option<ObjectId>)> {
        let dir = self.repo_path(a.repo.id);
        let r = &self.runner;
        // The prefix's commits are those of its changes; the squash takes
        // a change ID only if the prefix has one change.
        let prefix_ids: HashMap<String, String> = ids
            .iter()
            .filter(|(_, d)| p.changes.contains(d))
            .map(|(c, d)| (c.clone(), d.clone()))
            .collect();
        match strategy {
            "fast-forward" | "merge-commit" => {
                let text = message.map(str::to_owned).unwrap_or_else(|| {
                    format!(
                        "Merge !{} up to {}: {}",
                        m.number,
                        p.changes.last().map_or("", |d| d.get(..8).unwrap_or(d)),
                        m.title
                    )
                });
                let result = self
                    .build(
                        a,
                        m,
                        (strategy, t, top),
                        committer,
                        &prefix_ids,
                        Some(&text),
                        signatures,
                    )
                    .await?;
                Ok((result, None))
            }
            "rebase" => {
                let base = slath_git::browse::merge_base(r, &dir, t, head)
                    .await
                    .map_err(internal)?;
                let commits = self.stack_commits(&dir, base.as_ref(), head).await?;
                let new = self
                    .rebase_commits(m, &dir, &commits, t, committer, ids, signatures)
                    .await?;
                let at = commits
                    .iter()
                    .position(|c| c.oid == *top)
                    .ok_or_else(|| internal("the prefix isn't in the stack"))?;
                Ok((new[at].clone(), new.last().cloned()))
            }
            "squash" => {
                let squashed = self
                    .build(
                        a,
                        m,
                        ("squash", t, top),
                        committer,
                        &prefix_ids,
                        message,
                        signatures,
                    )
                    .await?;
                let commits = self.stack_commits(&dir, Some(top), head).await?;
                let new = self
                    .rebase_commits(m, &dir, &commits, &squashed, committer, ids, signatures)
                    .await?;
                Ok((squashed, new.last().cloned()))
            }
            other => Err(ServiceError::invalid(
                "strategy",
                "invalid",
                format!("Unknown strategy {other}."),
            )),
        }
    }

    /// The commits `base..head`, bottom first; refused with merges or too
    /// many to rebase.
    async fn stack_commits(
        &self,
        dir: &std::path::Path,
        base: Option<&ObjectId>,
        head: &ObjectId,
    ) -> Result<Vec<slath_git::browse::LogEntry>> {
        let max = self.config.limits.review_max_commits;
        let commits = slath_git::browse::log_range(&self.runner, dir, base, head, max + 1)
            .await
            .map_err(internal)?;
        if commits.len() > max || commits.iter().any(|c| c.parents.len() > 1) {
            return Err(ServiceError::invalid(
                "strategy",
                "not_rebasable",
                "This branch has merge commits or too many commits to rebase; merge or squash instead.",
            ));
        }
        Ok(commits)
    }
}
