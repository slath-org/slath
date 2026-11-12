// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Merge when ready: a merge request merges as soon as its requirements
//! are met, as the person who set it and with their strategy. Whatever
//! might meet a requirement (a status, a review, a resolved thread, a new
//! revision, marking it ready, the target moving) queues a check (job
//! `mr.auto_merge`). Taking the setting clears it in the same statement,
//! so it merges at most once. A new revision pushed by anyone else
//! cancels it, as do closing and cancelling by hand. The setter hears
//! what became of it.

use slath_core::authz::{Action, Subject};
use slath_core::ids::{MergeRequestId, RepoId, UserId};
use slath_db::merge_requests::{AutoMerge, MergeRequest};

use crate::Services;
use crate::error::{Result, ServiceError};
use crate::notifications::MrActivity;

/// The job that checks a merge request with merge when ready.
pub const MR_AUTO_MERGE: &str = "mr.auto_merge";

fn subject_user(subject: &Subject) -> Option<UserId> {
    match subject {
        Subject::User { id, .. } => Some(*id),
        _ => None,
    }
}

impl Services {
    /// The merge-when-ready setting of a merge request.
    pub async fn mr_auto_merge(&self, m: &MergeRequest) -> Result<Option<AutoMerge>> {
        Ok(slath_db::merge_requests::auto_merge(&self.pool, m.id).await?)
    }

    /// Whether the subject may cancel merge when ready: whoever set it, the
    /// author, and anyone who may set it.
    pub async fn may_cancel_auto_merge(
        &self,
        subject: &Subject,
        a: &crate::repos::RepoAccess,
        m: &MergeRequest,
        am: &AutoMerge,
    ) -> bool {
        let me = subject_user(subject);
        me.is_some() && (am.by_id == me || m.author_id == me || a.can(subject, Action::MrAutoMerge))
    }

    /// Sets merge when ready (`mr.auto_merge`) with `strategy` (default:
    /// the repository's). If the requirements are met already, it merges
    /// right away.
    pub async fn set_auto_merge(
        &self,
        subject: &Subject,
        (ns, repo, number): (&str, &str, i32),
        strategy: Option<&str>,
    ) -> Result<MergeRequest> {
        let (a, m) = self.mr(subject, ns, repo, number).await?;
        if !a.can(subject, Action::MrAutoMerge) {
            return Err(ServiceError::Forbidden);
        }
        self.check_bot_merge(subject, &a, &m.target_branch).await?;
        let user = subject_user(subject).ok_or(ServiceError::Unauthenticated)?;
        if m.state != "open" {
            return Err(ServiceError::Conflict(
                "The merge request isn't open.".into(),
            ));
        }
        let (allowed, default) = slath_db::repos::merge_settings(&self.pool, a.repo.id).await?;
        let strategy = strategy
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map_or(default, str::to_owned);
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
        let mut tx = self.pool.begin().await.map_err(ServiceError::internal)?;
        slath_db::merge_requests::set_auto_merge(&mut *tx, m.id, user, &strategy).await?;
        slath_db::issues::insert_event(
            &mut *tx,
            a.repo.id,
            "merge_request",
            *m.id.as_uuid(),
            Some(user),
            "auto_merge_set",
            &serde_json::json!({"strategy": strategy}),
        )
        .await?;
        tx.commit().await.map_err(ServiceError::internal)?;
        self.audit(
            subject,
            "mr.auto_merge",
            crate::audit::Target::new("merge_request", Some(*m.id.as_uuid()))
                .in_repo(*a.repo.id.as_uuid()),
            serde_json::json!({"number": m.number, "strategy": strategy}),
        )
        .await;
        self.try_auto_merge(m.id).await?;
        slath_db::merge_requests::get_by_id(&self.pool, m.id)
            .await?
            .ok_or(ServiceError::NotFound)
    }

    /// Cancels merge when ready by hand.
    pub async fn cancel_auto_merge(
        &self,
        subject: &Subject,
        (ns, repo, number): (&str, &str, i32),
    ) -> Result<()> {
        let (a, m) = self.mr(subject, ns, repo, number).await?;
        let Some(am) = self.mr_auto_merge(&m).await? else {
            return Ok(());
        };
        if !self.may_cancel_auto_merge(subject, &a, &m, &am).await {
            return Err(ServiceError::Forbidden);
        }
        self.end_auto_merge(&m, subject_user(subject), "manual", None)
            .await
    }

    /// Clears merge when ready and records why (`manual`, `push`, `closed`,
    /// `failed`, `no_permission`); the setter hears about it unless they
    /// did it themselves.
    pub(crate) async fn end_auto_merge(
        &self,
        m: &MergeRequest,
        actor: Option<UserId>,
        reason: &str,
        detail: Option<&str>,
    ) -> Result<()> {
        let mut tx = self.pool.begin().await.map_err(ServiceError::internal)?;
        let Some((_, setter)) = slath_db::merge_requests::take_auto_merge(&mut *tx, m.id).await?
        else {
            return Ok(());
        };
        slath_db::issues::insert_event(
            &mut *tx,
            m.repo_id,
            "merge_request",
            *m.id.as_uuid(),
            actor,
            "auto_merge_cancelled",
            &serde_json::json!({"reason": reason, "detail": detail}),
        )
        .await?;
        tx.commit().await.map_err(ServiceError::internal)?;
        if let Some(setter) = setter.filter(|s| Some(*s) != actor) {
            let outcome = match (reason, detail) {
                ("push", _) => {
                    "Merge when ready was cancelled: someone else pushed a new revision.".to_owned()
                }
                ("closed", _) => {
                    "Merge when ready was cancelled: the merge request was closed.".to_owned()
                }
                ("failed", Some(why)) => format!("Merging when ready failed: {why}"),
                ("no_permission", _) => {
                    "Merge when ready was cancelled: you can no longer merge it.".to_owned()
                }
                _ => "Merge when ready was cancelled.".to_owned(),
            };
            self.notify_mr(None, m, MrActivity::AutoMerge { setter, outcome })
                .await;
        }
        Ok(())
    }

    /// Queues a check of a merge request, if it has merge when ready set.
    pub(crate) async fn queue_auto_merge(&self, id: MergeRequestId) {
        match slath_db::merge_requests::get_by_id(&self.pool, id).await {
            Ok(Some(m)) if m.state == "open" => {}
            _ => return,
        }
        if !matches!(
            slath_db::merge_requests::auto_merge(&self.pool, id).await,
            Ok(Some(_))
        ) {
            return;
        }
        let job = slath_db::jobs::NewJob::new(
            "default",
            MR_AUTO_MERGE,
            serde_json::json!({"mr_id": id.to_string()}),
        )
        .dedupe_key(format!("{MR_AUTO_MERGE}:{}", uuid::Uuid::now_v7()));
        if let Err(e) = slath_db::jobs::enqueue(&self.pool, &job).await {
            tracing::warn!(error = %e, mr = %id, "queueing a merge-when-ready check failed");
        }
    }

    /// Queues checks of every merge request in a repository with merge when
    /// ready set (after a status or a target move).
    pub(crate) async fn queue_auto_merges_in(&self, repo: RepoId) {
        match slath_db::merge_requests::auto_merges(&self.pool, repo).await {
            Ok(ids) => {
                for id in ids {
                    self.queue_auto_merge(id).await;
                }
            }
            Err(e) => tracing::warn!(error = %e, "finding merge-when-ready merge requests failed"),
        }
    }

    /// The `mr.auto_merge` job.
    pub(crate) async fn run_auto_merge(
        &self,
        job: &slath_db::jobs::LeasedJob,
    ) -> std::result::Result<(), slath_db::worker::JobError> {
        use slath_db::worker::JobError;
        let id: MergeRequestId = job
            .payload
            .get("mr_id")
            .and_then(|v| v.as_str())
            .and_then(|s| s.parse().ok())
            .ok_or_else(|| JobError::Permanent("payload has no valid mr_id".to_owned()))?;
        self.try_auto_merge(id)
            .await
            .map(|_| ())
            .map_err(|e| JobError::Retry(e.to_string()))
    }

    /// Merges the merge request if merge when ready is set and every
    /// requirement is met now. Returns whether it merged.
    pub(crate) async fn try_auto_merge(&self, id: MergeRequestId) -> Result<bool> {
        let Some(m) = slath_db::merge_requests::get_by_id(&self.pool, id).await? else {
            return Ok(false);
        };
        if m.state != "open" {
            return Ok(false);
        }
        let Some(am) = self.mr_auto_merge(&m).await? else {
            return Ok(false);
        };
        // It merges as whoever set it, if they still may.
        let setter = match am.by_id {
            Some(u) => slath_db::users::get(&self.pool, u)
                .await?
                .map(|row| crate::notifications::user_subject(u, &row)),
            None => None,
        };
        let access = match &setter {
            Some(s) => match self.repo_access_by_id(s, m.repo_id, Action::RepoRead).await {
                Ok(a) if a.can(s, Action::MrMerge) => Some(a),
                Ok(_) | Err(ServiceError::NotFound | ServiceError::Forbidden) => None,
                Err(e) => return Err(e),
            },
            None => None,
        };
        let (Some(setter), Some(a)) = (setter, access) else {
            self.end_auto_merge(&m, None, "no_permission", None).await?;
            return Ok(false);
        };
        if !self.mr_mergeability(&a, &m).await?.mergeable {
            return Ok(false);
        }
        let Some(head) = m.head_oid.clone() else {
            return Ok(false);
        };
        // Only one check gets to merge.
        let Some((strategy, _)) =
            slath_db::merge_requests::take_auto_merge(&self.pool, m.id).await?
        else {
            return Ok(false);
        };
        let input = crate::merge::MergeInput {
            strategy: Some(strategy),
            expected_head_oid: head,
            commit_message: None,
            delete_source: None,
        };
        let setter_id = subject_user(&setter);
        match self
            .merge_mr(&setter, (&a.repo.namespace, &a.repo.name, m.number), &input)
            .await
        {
            Ok(merged) => {
                if let Some(u) = setter_id {
                    self.notify_mr(
                        None,
                        &merged,
                        MrActivity::AutoMerge {
                            setter: u,
                            outcome: format!(
                                "Merged when ready into {} as you asked.",
                                merged.target_branch
                            ),
                        },
                    )
                    .await;
                }
                Ok(true)
            }
            Err(e @ (ServiceError::Conflict(_) | ServiceError::Invalid(_))) => {
                let why = match &e {
                    ServiceError::Conflict(why) => why.clone(),
                    ServiceError::Invalid(errors) => errors
                        .iter()
                        .map(|f| f.message.clone())
                        .collect::<Vec<_>>()
                        .join(" "),
                    _ => String::new(),
                };
                slath_db::issues::insert_event(
                    &self.pool,
                    m.repo_id,
                    "merge_request",
                    *m.id.as_uuid(),
                    None,
                    "auto_merge_cancelled",
                    &serde_json::json!({"reason": "failed", "detail": why}),
                )
                .await?;
                if let Some(u) = setter_id {
                    self.notify_mr(
                        None,
                        &m,
                        MrActivity::AutoMerge {
                            setter: u,
                            outcome: format!("Merging when ready failed: {why}"),
                        },
                    )
                    .await;
                }
                Ok(false)
            }
            Err(e) => {
                // Not the merge request's fault: keep the setting for the retry.
                if let Some(u) = setter_id {
                    slath_db::merge_requests::set_auto_merge(&self.pool, m.id, u, &am.strategy)
                        .await?;
                }
                Err(e)
            }
        }
    }
}
