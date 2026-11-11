// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Merging merge requests on the server (spec: merge; D-12): fast-forward,
//! rebase, merge commit, and squash, built without a worktree. Rewritten
//! commits keep their headers and author, lose their signatures, and carry
//! their change's display ID (D-38). The target moves only by
//! compare-and-swap, retried a few times when someone pushes meanwhile.

use std::collections::HashMap;

use slath_core::authz::{Action, Subject};
use slath_db::merge_requests::MergeRequest;
use slath_git::ObjectId;
use slath_git::args::RefName;
use slath_git::merge::{self, TreeMerge};
use slath_git::refs::RefUpdateError;

use crate::Services;
use crate::error::{Result, ServiceError};
use crate::repos::RepoAccess;

/// How often a merge is rebuilt when the target moved meanwhile.
const ATTEMPTS: usize = 3;

/// What merging takes.
#[derive(Debug, Clone, Default)]
pub struct MergeInput {
    /// `fast-forward`, `rebase`, `merge-commit`, or `squash`; `None`: the
    /// repository's default.
    pub strategy: Option<String>,
    /// The head the person saw; a different current head is refused.
    pub expected_head_oid: String,
    /// For merge commits and squashes.
    pub commit_message: Option<String>,
    /// `None`: the merge request's setting.
    pub delete_source: Option<bool>,
}

fn internal(e: impl std::fmt::Display) -> ServiceError {
    ServiceError::internal(e.to_string())
}

fn oid(s: &str) -> Result<ObjectId> {
    ObjectId::parse(s).map_err(internal)
}

/// Commands that resolve conflicts with the target, for Git and jj.
#[must_use]
pub fn conflict_commands(source: &str, target: &str) -> (String, String) {
    (
        format!(
            "git fetch origin\ngit switch {source}\ngit rebase origin/{target}\n# resolve the conflicts, then: git add <files> && git rebase --continue\ngit push --force-with-lease origin {source}"
        ),
        format!(
            "jj git fetch\njj rebase -b {source} -d {target}@origin\n# resolve the conflicts (jj resolve), then:\njj git push --bookmark {source}"
        ),
    )
}

impl Services {
    /// `Name <username@users.noreply.host> <time> +0000` for commits the
    /// server writes on someone's behalf.
    pub(crate) async fn identity(&self, subject: &Subject) -> Result<String> {
        let Subject::User { id, .. } = subject else {
            return Err(ServiceError::Unauthenticated);
        };
        self.identity_of(*id).await
    }

    /// The commit identity of a user (see [`Self::identity`]).
    pub(crate) async fn identity_of(&self, id: slath_core::ids::UserId) -> Result<String> {
        let u = slath_db::users::get(&self.pool, id)
            .await?
            .ok_or(ServiceError::NotFound)?;
        let name: String = u
            .display_name
            .clone()
            .filter(|n| !n.trim().is_empty())
            .unwrap_or_else(|| u.name.clone())
            .chars()
            .filter(|c| !matches!(c, '<' | '>' | '\n') && !c.is_control())
            .collect();
        let host = self.config.server.public_url.host();
        let now = time::OffsetDateTime::now_utc().unix_timestamp();
        Ok(format!(
            "{} <{}@users.noreply.{host}> {now} +0000",
            name.trim(),
            u.name
        ))
    }

    /// The display ID of each commit's change in the current revision, for
    /// the change-id header of rewritten commits.
    pub(crate) async fn change_ids(&self, m: &MergeRequest) -> Result<HashMap<String, String>> {
        let changes: HashMap<uuid::Uuid, String> = slath_db::mr_changes::changes(&self.pool, m.id)
            .await?
            .into_iter()
            .map(|c| (c.id, c.display_id))
            .collect();
        Ok(slath_db::mr_changes::versions(&self.pool, m.id)
            .await?
            .into_iter()
            .filter(|v| v.revision_seq == m.revisions)
            .filter_map(|v| Some((v.commit_oid, changes.get(&v.change_id?)?.clone())))
            .collect())
    }

    /// The strategies the repository allows, and its default.
    pub async fn mr_merge_settings(&self, a: &RepoAccess) -> Result<(Vec<String>, String)> {
        Ok(slath_db::repos::merge_settings(&self.pool, a.repo.id).await?)
    }

    /// Whether the merge request's head merges into the target without
    /// conflicts; the conflicting paths otherwise.
    pub async fn mr_conflicts(&self, m: &MergeRequest) -> Result<Vec<String>> {
        let Some(head) = m.head_oid.as_deref() else {
            return Ok(Vec::new());
        };
        let dir = self.repo_path(m.repo_id);
        let target =
            RefName::branch(m.target_branch.as_bytes()).map_err(|_| internal("bad target"))?;
        let Some(t) = slath_git::refs::read(&self.runner, &dir, &target)
            .await
            .map_err(internal)?
        else {
            return Ok(Vec::new());
        };
        let head = oid(head)?;
        if merge::is_ancestor(&self.runner, &dir, &t, &head)
            .await
            .map_err(internal)?
        {
            return Ok(Vec::new());
        }
        match merge::merge_trees(&self.runner, &dir, None, &t, &head)
            .await
            .map_err(internal)?
        {
            TreeMerge::Clean(_) => Ok(Vec::new()),
            TreeMerge::Conflicts(files) => Ok(files),
        }
    }

    /// Merges a merge request: checks the head and the requirements,
    /// builds the result, and moves the target by compare-and-swap.
    pub async fn merge_mr(
        &self,
        subject: &Subject,
        (ns, repo, number): (&str, &str, i32),
        input: &MergeInput,
    ) -> Result<MergeRequest> {
        let (a, m) = self.mr(subject, ns, repo, number).await?;
        if !a.can(subject, Action::MrMerge) {
            return Err(ServiceError::Forbidden);
        }
        let user = match subject {
            Subject::User { id, .. } => *id,
            _ => return Err(ServiceError::Unauthenticated),
        };
        if m.state != "open" {
            return Err(ServiceError::Conflict(
                "The merge request isn't open.".into(),
            ));
        }
        if m.head_oid.as_deref() != Some(input.expected_head_oid.as_str()) {
            return Err(ServiceError::Conflict(format!(
                "The merge request changed since you looked: it's at revision {} now.",
                m.revisions
            )));
        }
        let (allowed, default) = slath_db::repos::merge_settings(&self.pool, a.repo.id).await?;
        let strategy = input.strategy.clone().unwrap_or(default);
        if !allowed.contains(&strategy) {
            return Err(ServiceError::invalid(
                "strategy",
                "not_allowed",
                format!(
                    "This repository doesn't allow {strategy}; use {}.",
                    allowed.join(", ")
                ),
            ));
        }
        // Push protection scans what a merge writes that wasn't pushed: the
        // message of merge commits and squashes.
        if strategy == "merge-commit" || strategy == "squash" {
            self.scan_message(&a, &m, &strategy, input.commit_message.as_deref())
                .await?;
        }
        let mb = self.mr_mergeability(&a, &m).await?;
        if !mb.mergeable {
            let why: Vec<String> = mb
                .requirements
                .iter()
                .filter(|r| r.state != "met")
                .map(|r| r.reason.clone())
                .collect();
            return Err(ServiceError::Conflict(format!(
                "It can't be merged yet: {}",
                why.join(" ")
            )));
        }
        let committer = self.identity(subject).await?;
        let dir = self.repo_path(a.repo.id);
        let target =
            RefName::branch(m.target_branch.as_bytes()).map_err(|_| internal("bad target"))?;
        let head = oid(&input.expected_head_oid)?;
        let ids = self.change_ids(&m).await?;
        let mut tx = self.pool.begin().await.map_err(internal)?;
        slath_db::merge_requests::lock_target(&mut *tx, a.repo.id, &m.target_branch).await?;
        // Another merge may have finished while this one waited for the lock.
        match slath_db::merge_requests::get_by_id(&mut *tx, m.id).await? {
            Some(now) if now.state == "open" && now.head_oid == m.head_oid => {}
            _ => {
                return Err(ServiceError::Conflict(
                    "The merge request changed while merging; reload it.".into(),
                ));
            }
        }
        let mut landed = None;
        // Signed commits a rebase rewrote: (original, new).
        let mut signatures: Vec<(String, String)> = Vec::new();
        for _ in 0..ATTEMPTS {
            let t = slath_git::refs::read(&self.runner, &dir, &target)
                .await
                .map_err(internal)?
                .ok_or(ServiceError::Conflict(
                    "The target branch doesn't exist.".into(),
                ))?;
            signatures.clear();
            let result = self
                .build(
                    &a,
                    &m,
                    (&strategy, &t, &head),
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
                    landed = Some((t, result));
                    break;
                }
                Err(RefUpdateError::Conflict { .. }) => {}
                Err(e) => return Err(internal(e)),
            }
        }
        let Some((old, result)) = landed else {
            return Err(ServiceError::Conflict(
                "The target branch kept moving while merging; try again.".into(),
            ));
        };
        slath_db::merge_requests::set_merged(
            &mut *tx,
            m.id,
            Some(user),
            &strategy,
            result.as_str(),
        )
        .await?;
        slath_db::mr_changes::set_merged(&mut *tx, m.id, m.revisions).await?;
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
            "merged",
            &serde_json::json!({"strategy": strategy, "oid": result.as_str(), "target": m.target_branch}),
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
        self.sync_mr_branch_by_id(m.id).await;
        if input.delete_source.unwrap_or(m.delete_source_on_merge) {
            self.delete_source(subject, &a, &m, &head).await;
        }
        self.notify_mr(Some(user), &m, crate::notifications::MrActivity::Merged)
            .await;
        self.audit(
            subject,
            "mr.merge",
            crate::audit::Target::new("merge_request", Some(*m.id.as_uuid()))
                .in_repo(*a.repo.id.as_uuid()),
            serde_json::json!({"number": m.number, "strategy": strategy, "oid": result.as_str()}),
        )
        .await;
        slath_db::merge_requests::get_by_id(&self.pool, m.id)
            .await?
            .ok_or(ServiceError::NotFound)
    }

    /// Refuses a merge or squash message with secrets that block rules
    /// find, unless the repository's push protection is off or warns.
    async fn scan_message(
        &self,
        a: &RepoAccess,
        m: &MergeRequest,
        strategy: &str,
        message: Option<&str>,
    ) -> Result<()> {
        let mode = self.push_protection_mode(a.repo.id).await?;
        if mode != slath_core::config::ScanMode::Block {
            return Ok(());
        }
        let text = message
            .map(str::to_owned)
            .filter(|t| !t.trim().is_empty())
            .unwrap_or_else(|| {
                if strategy == "merge-commit" {
                    format!("Merge !{}: {}", m.number, m.title)
                } else {
                    format!("{}\n\n{}", m.title, m.description)
                }
            });
        let blocking: Vec<String> = self
            .scan_rules
            .scan(text.as_bytes())
            .into_iter()
            .filter(|f| f.tier == slath_core::secret_scan::Tier::Block)
            .map(|f| f.description)
            .collect();
        if blocking.is_empty() {
            return Ok(());
        }
        Err(ServiceError::invalid(
            "commit_message",
            "secret",
            format!(
                "The commit message contains what looks like a secret ({}); remove it, or write another message.",
                blocking.join(", ")
            ),
        ))
    }

    /// The commit the target moves to.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn build(
        &self,
        a: &RepoAccess,
        m: &MergeRequest,
        (strategy, t, head): (&str, &ObjectId, &ObjectId),
        committer: &str,
        ids: &HashMap<String, String>,
        message: Option<&str>,
        signatures: &mut Vec<(String, String)>,
    ) -> Result<ObjectId> {
        let dir = self.repo_path(a.repo.id);
        let r = &self.runner;
        let conflict = |files: Vec<String>| {
            ServiceError::Conflict(format!(
                "Merging into {} conflicts in: {}.",
                m.target_branch,
                files.join(", ")
            ))
        };
        let ff = merge::is_ancestor(r, &dir, t, head)
            .await
            .map_err(internal)?;
        let message = message.map(str::trim).filter(|s| !s.is_empty());
        match strategy {
            "fast-forward" => {
                if !ff {
                    return Err(ServiceError::invalid(
                        "strategy",
                        "not_fast_forward",
                        format!(
                            "{} has commits the branch doesn't; rebase, merge, or squash instead.",
                            m.target_branch
                        ),
                    ));
                }
                Ok(head.clone())
            }
            "rebase" => {
                let base = slath_git::browse::merge_base(r, &dir, t, head)
                    .await
                    .map_err(internal)?;
                let commits = slath_git::browse::log_range(
                    r,
                    &dir,
                    base.as_ref(),
                    head,
                    self.config.limits.review_max_commits + 1,
                )
                .await
                .map_err(internal)?;
                if commits.len() > self.config.limits.review_max_commits
                    || commits.iter().any(|c| c.parents.len() > 1)
                {
                    return Err(ServiceError::invalid(
                        "strategy",
                        "not_rebasable",
                        "This branch has merge commits or too many commits to rebase; merge or squash instead.",
                    ));
                }
                let mut reader = slath_git::ObjectReader::open(r, &dir)
                    .await
                    .map_err(internal)?;
                let mut onto = t.clone();
                for c in &commits {
                    let raw = reader.read_raw_commit(&c.oid).await.map_err(internal)?;
                    let tree = match merge::merge_trees(r, &dir, c.parents.first(), &onto, &c.oid)
                        .await
                        .map_err(internal)?
                    {
                        TreeMerge::Clean(tree) => tree,
                        TreeMerge::Conflicts(files) => return Err(conflict(files)),
                    };
                    let new = merge::rewrite(
                        &raw,
                        &tree,
                        std::slice::from_ref(&onto),
                        committer,
                        ids.get(c.oid.as_str()).map(String::as_str),
                    );
                    onto = merge::write_commit(r, &dir, &new).await.map_err(internal)?;
                    if raw.header(b"gpgsig").is_some() || raw.header(b"gpgsig-sha256").is_some() {
                        signatures.push((c.oid.as_str().to_owned(), onto.as_str().to_owned()));
                    }
                }
                Ok(onto)
            }
            "merge-commit" | "squash" => {
                let tree = match merge::merge_trees(r, &dir, None, t, head)
                    .await
                    .map_err(internal)?
                {
                    TreeMerge::Clean(tree) => tree,
                    TreeMerge::Conflicts(files) => return Err(conflict(files)),
                };
                if strategy == "merge-commit" {
                    let text = message.map_or_else(
                        || format!("Merge !{}: {}", m.number, m.title),
                        str::to_owned,
                    );
                    let c = merge::new_commit(
                        &tree,
                        &[t.clone(), head.clone()],
                        committer,
                        committer,
                        None,
                        &text,
                    );
                    return merge::write_commit(r, &dir, &c).await.map_err(internal);
                }
                // Squash: the author of the first commit, one change ID.
                let base = slath_git::browse::merge_base(r, &dir, t, head)
                    .await
                    .map_err(internal)?;
                let commits = slath_git::browse::log_range(r, &dir, base.as_ref(), head, 1_000_000)
                    .await
                    .map_err(internal)?;
                let first = commits
                    .first()
                    .map_or_else(|| head.clone(), |c| c.oid.clone());
                let mut reader = slath_git::ObjectReader::open(r, &dir)
                    .await
                    .map_err(internal)?;
                let author = reader
                    .read_raw_commit(&first)
                    .await
                    .map_err(internal)?
                    .header(b"author")
                    .map(|a| String::from_utf8_lossy(a).into_owned())
                    .unwrap_or_else(|| committer.to_owned());
                let distinct: std::collections::BTreeSet<&String> = ids.values().collect();
                let change_id = match distinct.into_iter().collect::<Vec<_>>().as_slice() {
                    [one] => (*one).clone(),
                    _ => {
                        let mut bytes = [0u8; 16];
                        getrandom::fill(&mut bytes).map_err(internal)?;
                        slath_core::change_identity::id_from_bytes(&bytes)
                    }
                };
                let text = message.map_or_else(
                    || {
                        if m.description.trim().is_empty() {
                            m.title.clone()
                        } else {
                            format!("{}\n\n{}", m.title, m.description.trim())
                        }
                    },
                    str::to_owned,
                );
                let c = merge::new_commit(
                    &tree,
                    std::slice::from_ref(t),
                    &author,
                    committer,
                    Some(&change_id),
                    &text,
                );
                merge::write_commit(r, &dir, &c).await.map_err(internal)
            }
            other => Err(ServiceError::invalid(
                "strategy",
                "invalid",
                format!("Unknown strategy {other}."),
            )),
        }
    }

    /// Deletes the source branch after a merge, if the merger can push it
    /// and it isn't protected from deletion. A fork's branch is its owner's
    /// and stays. Failures only cost the delete.
    async fn delete_source(
        &self,
        subject: &Subject,
        a: &RepoAccess,
        m: &MergeRequest,
        head: &ObjectId,
    ) {
        if m.source_repo_id != Some(a.repo.id) || !a.can(subject, Action::RepoPush) {
            return;
        }
        let Some(branch) = m.source_ref.strip_prefix(b"refs/heads/") else {
            return;
        };
        if let Ok(rules) = slath_db::protections::list(&self.pool, a.repo.id).await
            && crate::protections::effective(&rules, branch).block_deletion
        {
            return;
        }
        let Ok(name) = RefName::parse(m.source_ref.clone()) else {
            return;
        };
        let dir = self.repo_path(a.repo.id);
        if let Err(e) =
            slath_git::refs::update_cas(&self.runner, &dir, &name, None, Some(head)).await
        {
            tracing::info!(error = %e, "the source branch wasn't deleted after the merge");
            return;
        }
        let user = match subject {
            Subject::User { id, .. } => Some(*id),
            _ => None,
        };
        let zero = "0".repeat(40);
        if let Err(e) = self
            .record_push(
                a.repo.id,
                user,
                "internal",
                &[slath_git::ipc::RefUpdate {
                    name: m.source_ref.clone(),
                    old: head.as_str().to_owned(),
                    new: zero,
                }],
            )
            .await
        {
            tracing::warn!(error = %e, "recording the deleted source branch failed");
        }
    }
}
