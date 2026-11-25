// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! One call per iteration of an author or agent: the current revision,
//! what changed since an earlier one, failing required checks, unresolved
//! threads, and mergeability. Composed from the calls behind the
//! individual endpoints, so it never says more than they do.

use slath_core::authz::Subject;
use slath_db::merge_requests::Revision;
use slath_db::statuses::Status;

use crate::Services;
use crate::error::{Result, ServiceError};
use crate::mr_reviews::Mergeability;
use crate::mr_threads::ThreadView;

/// The revision interdiff since an earlier revision.
#[derive(Debug, Clone)]
pub struct IterationInterdiff {
    pub from: i32,
    pub to: i32,
    /// `None` past `limits.iteration_interdiff_max_bytes`.
    pub text: Option<String>,
    pub too_large: bool,
}

/// What an iteration needs, for one merge request.
pub struct Iteration {
    /// `None` before the first revision.
    pub revision: Option<Revision>,
    /// `None` without `since`, when `since` is the current revision, or
    /// when either revision has no merge base.
    pub interdiff: Option<IterationInterdiff>,
    /// The latest status of each required context on the head, when it
    /// failed (`failure` or `error`).
    pub failing_checks: Vec<Status>,
    /// The unresolved threads the subject may see.
    pub threads: Vec<ThreadView>,
    pub mergeability: Mergeability,
    /// The work order the merge request belongs to, if any.
    pub work_order: Option<crate::work_orders::OrderView>,
}

impl Services {
    /// The iteration view of a merge request (whoever may read it).
    /// `since` must name one of its revisions.
    pub async fn mr_iteration(
        &self,
        subject: &Subject,
        (ns, repo, number): (&str, &str, i32),
        since: Option<i32>,
    ) -> Result<Iteration> {
        let (a, m) = self.mr(subject, ns, repo, number).await?;
        if let Some(k) = since
            && !(1..=m.revisions).contains(&k)
        {
            return Err(ServiceError::invalid(
                "since",
                "unknown_revision",
                format!(
                    "The merge request has revisions 1 to {}; {k} isn't one of them.",
                    m.revisions
                ),
            ));
        }
        let revision = self.mr_revisions(&m).await?.pop();
        let interdiff = match since {
            Some(k) if k < m.revisions => self
                .mr_revision_interdiff(&m, k, m.revisions)
                .await?
                .map(|(raw, cut)| {
                    let max = self.config.limits.iteration_interdiff_max_bytes.as_usize();
                    let too_large = cut || raw.len() > max;
                    IterationInterdiff {
                        from: k,
                        to: m.revisions,
                        text: (!too_large).then(|| String::from_utf8_lossy(&raw).into_owned()),
                        too_large,
                    }
                }),
            _ => None,
        };
        let required = self.mr_required_contexts(&m).await?;
        let failing_checks = self
            .mr_statuses(&m)
            .await?
            .into_iter()
            .filter(|s| {
                required.contains(&s.context) && matches!(s.state.as_str(), "failure" | "error")
            })
            .collect();
        let threads = self
            .mr_threads(subject, &a, &m)
            .await?
            .into_iter()
            .filter(|t| t.thread.resolved_at.is_none())
            .collect();
        let mergeability = self.mr_mergeability(&a, &m).await?;
        let work_order = match self.order_of_mr(&m).await? {
            Some(o) => Some(self.order_view(&a, self.expire_if_due(o).await?).await?),
            None => None,
        };
        Ok(Iteration {
            revision,
            interdiff,
            failing_checks,
            threads,
            mergeability,
            work_order,
        })
    }
}
