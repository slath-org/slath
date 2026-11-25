// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Work orders: a person with write access delegates an issue, or a
//! follow-up on a merge request, to a bot.
//!
//! The order names the bot, the goal, optional instructions and path
//! scope, a budget (revisions, open merge requests, expiry), required
//! checks, and reviewers. The bot claims it for a short-lived task token
//! that may push only branches under `wo/<n>/` and work on the order's
//! issue and merge requests. Each decision about something done under the
//! order is one of its events, kept as long as the repository.
//!
//! The bot must hold the write role on the repository itself: a task
//! token is the bot's own role narrowed to the order, never anyone else's.

use serde::Deserialize;
use slath_core::Secret;
use slath_core::authz::{Action, Credential, Role, Subject, UserKind, UserState};
use slath_core::credential::CredentialKind;
use slath_core::ids::UserId;
use slath_core::sensitive_paths::invalid_glob;
use slath_db::work_orders::{Event, NewWorkOrder, WorkOrder};
use time::OffsetDateTime;

use crate::Services;
use crate::audit::Target;
use crate::error::{FieldError, Result, ServiceError};
use crate::repos::RepoAccess;

/// The longest goal, in bytes.
pub const GOAL_MAX_BYTES: usize = 16 * 1024;
/// The longest instructions, in bytes.
pub const INSTRUCTIONS_MAX_BYTES: usize = 64 * 1024;
/// Most path globs in a scope.
pub const SCOPE_MAX: usize = 50;
/// Most required status contexts.
pub const CHECKS_MAX: usize = 20;
/// Most reviewers.
pub const REVIEWERS_MAX: usize = 10;
/// Most open merge requests one order may have.
pub const OPEN_MRS_MAX: i32 = 10;
/// Defaults: revisions, open merge requests, days to run.
pub const DEFAULT_REVISIONS: i32 = 20;
pub const DEFAULT_OPEN_MRS: i32 = 1;
pub const DEFAULT_DAYS: i64 = 7;

/// What an order is for: an issue or a merge request of its repository.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum TargetRef {
    Issue(i32),
    MergeRequest(i32),
}

/// An order as requested; the bot is named, the budget optional.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NewOrder {
    pub target: TargetRef,
    pub bot: String,
    pub goal: String,
    #[serde(default)]
    pub instructions: Option<String>,
    #[serde(default)]
    pub scope: Vec<String>,
    #[serde(default)]
    pub max_revisions: Option<i32>,
    #[serde(default)]
    pub max_open_mrs: Option<i32>,
    /// Days until the order ends at the latest.
    #[serde(default)]
    pub expires_in_days: Option<i64>,
    #[serde(default)]
    pub required_checks: Vec<String>,
    /// Reviewers by name.
    #[serde(default)]
    pub reviewers: Vec<String>,
}

/// An order as its readers see it.
#[derive(Debug, Clone)]
pub struct OrderView {
    pub order: WorkOrder,
    pub namespace: String,
    pub repo: String,
    /// The target's number and title.
    pub target_number: i32,
    pub target_title: String,
    pub bot: String,
    pub creator: Option<String>,
    pub reviewers: Vec<String>,
    pub ended_by: Option<String>,
}

impl OrderView {
    /// The branch prefix the order's task token may push under.
    #[must_use]
    pub fn branch_prefix(&self) -> String {
        branch_prefix(self.order.number)
    }
}

/// A task token from a claim, shown once.
#[derive(Debug)]
pub struct Claim {
    pub token: Secret<String>,
    pub expires_at: OffsetDateTime,
}

/// `wo/<n>/`.
#[must_use]
pub fn branch_prefix(number: i32) -> String {
    format!("wo/{number}/")
}

fn human(subject: &Subject) -> Result<UserId> {
    match subject {
        Subject::User {
            id,
            kind: UserKind::Human,
            state: UserState::Active,
            ..
        } => Ok(*id),
        Subject::User { .. } => Err(ServiceError::Forbidden),
        _ => Err(ServiceError::Unauthenticated),
    }
}

fn text_field(
    field: &str,
    text: Option<&str>,
    max: usize,
    errors: &mut Vec<FieldError>,
) -> Option<String> {
    let t = text.map(str::trim_end).filter(|t| !t.trim().is_empty())?;
    if t.len() > max {
        errors.push(FieldError::new(
            field,
            "too_long",
            format!("Keep it to {} KiB.", max / 1024),
        ));
    }
    Some(t.to_owned())
}

impl Services {
    /// The bot `name` if it may take orders in `a`'s repository: an active
    /// bot with the write role there (its effective role, capped by its
    /// owner's).
    async fn order_bot(&self, a: &RepoAccess, name: &str) -> Result<slath_db::users::User> {
        let refuse = || {
            ServiceError::invalid(
                "bot",
                "invalid",
                "Choose an active bot with write access to this repository.",
            )
        };
        let bot = slath_db::users::get_by_name(&self.pool, name.trim())
            .await?
            .filter(|u| u.kind == UserKind::Bot && u.state == UserState::Active)
            .ok_or_else(refuse)?;
        let subject = crate::notifications::user_subject(bot.id, &bot);
        let role = self.effective_role(&subject, &a.repo).await?;
        if role.is_some_and(|r| r >= Role::Write) {
            Ok(bot)
        } else {
            Err(refuse())
        }
    }

    /// The bots the subject could delegate to in `a`'s repository, by name.
    pub async fn order_bots(&self, a: &RepoAccess) -> Result<Vec<String>> {
        let mut out = Vec::new();
        for name in slath_db::bots::names_with_access(&self.pool, a.repo.id).await? {
            if self.order_bot(a, &name).await.is_ok() {
                out.push(name);
            }
        }
        Ok(out)
    }

    /// Delegates an issue or a merge request to a bot. People with write
    /// access only; the target must be open and have no open order.
    pub async fn create_work_order(
        &self,
        subject: &Subject,
        (ns, repo): (&str, &str),
        o: &NewOrder,
    ) -> Result<OrderView> {
        let creator = human(subject)?;
        let a = self
            .repo_access(subject, ns, repo, Action::WorkOrderCreate)
            .await?;
        // The target, readable and open.
        let (issue_id, mr_id) = match o.target {
            TargetRef::Issue(n) => {
                let i = slath_db::issues::get(&self.pool, a.repo.id, n)
                    .await?
                    .ok_or_else(|| {
                        ServiceError::invalid(
                            "/target",
                            "invalid",
                            "Choose an issue of this repository.",
                        )
                    })?;
                if i.state != "open" {
                    return Err(ServiceError::Conflict(
                        "This issue is closed, so it can't be delegated.".into(),
                    ));
                }
                (Some(i.id), None)
            }
            TargetRef::MergeRequest(n) => {
                let m = slath_db::merge_requests::get(&self.pool, a.repo.id, n)
                    .await?
                    .ok_or_else(|| {
                        ServiceError::invalid(
                            "/target",
                            "invalid",
                            "Choose a merge request of this repository.",
                        )
                    })?;
                if m.state != "open" {
                    return Err(ServiceError::Conflict(
                        "This merge request isn't open, so it can't be delegated.".into(),
                    ));
                }
                (None, Some(*m.id.as_uuid()))
            }
        };
        let bot = self.order_bot(&a, &o.bot).await?;

        let mut errors = Vec::new();
        let goal = text_field("/goal", Some(&o.goal), GOAL_MAX_BYTES, &mut errors);
        if goal.is_none() {
            errors.push(FieldError::new(
                "/goal",
                "required",
                "Say what the bot should do.",
            ));
        }
        let instructions = text_field(
            "/instructions",
            o.instructions.as_deref(),
            INSTRUCTIONS_MAX_BYTES,
            &mut errors,
        );
        let scope: Vec<String> = o
            .scope
            .iter()
            .map(|g| g.trim().to_owned())
            .filter(|g| !g.is_empty())
            .collect();
        if scope.len() > SCOPE_MAX {
            errors.push(FieldError::new(
                "/scope",
                "too_many",
                format!("Name at most {SCOPE_MAX} paths."),
            ));
        }
        for g in &scope {
            if let Some(why) = invalid_glob(g) {
                errors.push(FieldError::new(
                    "/scope",
                    "invalid",
                    format!("{g:?} {why}."),
                ));
            }
        }
        let limits = &self.config.limits;
        let max_revisions = o.max_revisions.unwrap_or(DEFAULT_REVISIONS);
        let rev_max = i32::try_from(limits.work_order_max_revisions).unwrap_or(i32::MAX);
        if !(1..=rev_max).contains(&max_revisions) {
            errors.push(FieldError::new(
                "/max_revisions",
                "invalid",
                format!("Allow between 1 and {rev_max} revisions."),
            ));
        }
        let max_open_mrs = o.max_open_mrs.unwrap_or(DEFAULT_OPEN_MRS);
        if !(1..=OPEN_MRS_MAX).contains(&max_open_mrs) {
            errors.push(FieldError::new(
                "/max_open_mrs",
                "invalid",
                format!("Allow between 1 and {OPEN_MRS_MAX} open merge requests."),
            ));
        }
        let days = o.expires_in_days.unwrap_or(DEFAULT_DAYS);
        let max_days = i64::from(limits.work_order_max_days);
        if !(1..=max_days).contains(&days) {
            errors.push(FieldError::new(
                "/expires_in_days",
                "invalid",
                format!("Let it run between 1 and {max_days} days."),
            ));
        }
        let expires_at = OffsetDateTime::now_utc() + time::Duration::days(days.clamp(1, max_days));
        let checks: Vec<String> = o
            .required_checks
            .iter()
            .map(|c| c.trim().to_owned())
            .filter(|c| !c.is_empty())
            .collect();
        if checks.len() > CHECKS_MAX
            || checks
                .iter()
                .any(|c| c.len() > 255 || c.chars().any(char::is_control))
        {
            errors.push(FieldError::new(
                "/required_checks",
                "invalid",
                format!("Name at most {CHECKS_MAX} status contexts of up to 255 bytes."),
            ));
        }
        let mut reviewer_ids = Vec::new();
        if o.reviewers.len() > REVIEWERS_MAX {
            errors.push(FieldError::new(
                "/reviewers",
                "too_many",
                format!("Name at most {REVIEWERS_MAX} reviewers."),
            ));
        }
        for name in o
            .reviewers
            .iter()
            .map(|r| r.trim())
            .filter(|r| !r.is_empty())
        {
            let user = slath_db::users::get_by_name(&self.pool, name)
                .await?
                .filter(|u| u.kind == UserKind::Human && u.state == UserState::Active);
            let readable = match &user {
                Some(u) => {
                    let s = crate::notifications::user_subject(u.id, u);
                    self.effective_role(&s, &a.repo).await?.is_some()
                        || a.repo.visibility != slath_core::authz::Visibility::Private
                }
                None => false,
            };
            match user {
                Some(u) if readable => {
                    if !reviewer_ids.contains(u.id.as_uuid()) {
                        reviewer_ids.push(*u.id.as_uuid());
                    }
                }
                _ => errors.push(FieldError::new(
                    "/reviewers",
                    "invalid",
                    format!("{name:?} isn't a person who can read this repository."),
                )),
            }
        }
        if !errors.is_empty() {
            return Err(ServiceError::Invalid(errors));
        }
        let goal = goal.unwrap_or_default();
        let mut texts = vec![("/goal".to_owned(), goal.as_str())];
        if let Some(i) = &instructions {
            texts.push(("/instructions".to_owned(), i.as_str()));
        }
        self.refuse_secrets(texts)?;

        let origin = self.origin_of(subject, &a.repo).await?;
        let mut tx = self.pool.begin().await.map_err(ServiceError::internal)?;
        let order = slath_db::work_orders::insert(
            &mut tx,
            &NewWorkOrder {
                repo_id: a.repo.id,
                target_kind: match o.target {
                    TargetRef::Issue(_) => "issue",
                    TargetRef::MergeRequest(_) => "merge_request",
                },
                issue_id,
                mr_id,
                bot_id: bot.id,
                creator_id: creator,
                origin: origin.as_str(),
                goal: &goal,
                instructions: instructions.as_deref(),
                scope: &scope,
                max_revisions,
                max_open_mrs,
                expires_at,
                required_checks: &checks,
                reviewer_ids: &reviewer_ids,
            },
        )
        .await
        .map_err(|e| match e {
            slath_db::DbError::Conflict(_) => ServiceError::Conflict(
                "This already has an open work order. Stop it first to delegate again.".into(),
            ),
            e => e.into(),
        })?;
        slath_db::work_orders::insert_event(
            &mut *tx,
            order.id,
            ("work_order.create", "allowed", None),
            None,
            Some(creator),
        )
        .await?;
        tx.commit().await.map_err(ServiceError::internal)?;
        self.audit(
            subject,
            "work_order.create",
            Target::new("work_order", Some(order.id)).in_repo(*a.repo.id.as_uuid()),
            serde_json::json!({"number": order.number, "bot": bot.name}),
        )
        .await;
        self.order_view(&a, order).await
    }

    /// The view of `order`, with names resolved.
    pub(crate) async fn order_view(&self, a: &RepoAccess, order: WorkOrder) -> Result<OrderView> {
        let name = |id: Option<UserId>| async move {
            Ok::<_, ServiceError>(match id {
                Some(id) => slath_db::users::get(&self.pool, id).await?.map(|u| u.name),
                None => None,
            })
        };
        let (target_number, target_title) = match (order.issue_id, order.mr_id) {
            (Some(i), _) => slath_db::issues::get_by_id(&self.pool, i)
                .await?
                .map(|i| (i.number, i.title))
                .unwrap_or_default(),
            (_, Some(m)) => slath_db::merge_requests::get_by_id(
                &self.pool,
                slath_core::ids::MergeRequestId::from_uuid(m),
            )
            .await?
            .map(|m| (m.number, m.title))
            .unwrap_or_default(),
            _ => (0, String::new()),
        };
        let mut reviewers = Vec::new();
        for id in &order.reviewer_ids {
            if let Some(n) = name(Some(UserId::from_uuid(*id))).await? {
                reviewers.push(n);
            }
        }
        Ok(OrderView {
            namespace: a.repo.namespace.clone(),
            repo: a.repo.name.clone(),
            target_number,
            target_title,
            bot: name(Some(order.bot_id)).await?.unwrap_or_default(),
            creator: name(order.creator_id).await?,
            ended_by: name(order.ended_by).await?,
            reviewers,
            order,
        })
    }

    /// Whether the subject is `order`'s bot, with a credential that may
    /// claim (so it may read the order without reading the repository).
    fn is_claimer(subject: &Subject, a: &RepoAccess, order: &WorkOrder) -> bool {
        matches!(subject, Subject::User { id, .. } if *id == order.bot_id)
            && a.can(subject, Action::WorkOrderClaim)
    }

    /// The repository and order `number` for a reader: everyone who can
    /// read the repository, and the order's bot with its claim credential.
    pub async fn work_order(
        &self,
        subject: &Subject,
        (ns, repo): (&str, &str),
        number: i32,
    ) -> Result<(RepoAccess, OrderView)> {
        let a = match self.resolve_repo(subject, ns, repo).await? {
            crate::repos::Resolved::Repo(a) => *a,
            crate::repos::Resolved::Moved { namespace, name } => {
                return Err(ServiceError::Moved { namespace, name });
            }
        };
        let order = slath_db::work_orders::get(&self.pool, a.repo.id, number).await?;
        let readable = a.can(subject, Action::RepoRead)
            || order
                .as_ref()
                .is_some_and(|o| Self::is_claimer(subject, &a, o));
        match order {
            Some(o) if readable => {
                let o = self.expire_if_due(o).await?;
                let view = self.order_view(&a, o).await?;
                Ok((a, view))
            }
            // A token without the read scope reaches nothing here.
            _ if !readable => Err(ServiceError::Forbidden),
            _ => Err(ServiceError::NotFound),
        }
    }

    /// A repository's orders, newest first (`state` `open` or `ended`).
    pub async fn work_orders(
        &self,
        subject: &Subject,
        (ns, repo): (&str, &str),
        state: Option<&str>,
        offset: i64,
        limit: i64,
    ) -> Result<Vec<OrderView>> {
        let a = self
            .repo_access(subject, ns, repo, Action::RepoRead)
            .await?;
        if state.is_some_and(|s| !matches!(s, "open" | "ended")) {
            return Err(ServiceError::invalid(
                "state",
                "invalid",
                "Choose open or ended.",
            ));
        }
        let rows = slath_db::work_orders::list(
            &self.pool,
            a.repo.id,
            state,
            offset.max(0),
            limit.clamp(1, 201),
        )
        .await?;
        let mut out = Vec::with_capacity(rows.len());
        for o in rows {
            let o = self.expire_if_due(o).await?;
            out.push(self.order_view(&a, o).await?);
        }
        Ok(out)
    }

    /// The open orders assigned to the calling bot, in the repositories its
    /// token names (the pull API).
    pub async fn assigned_work_orders(&self, subject: &Subject) -> Result<Vec<OrderView>> {
        let (bot, repos) = match subject {
            Subject::User {
                id,
                kind: UserKind::Bot,
                state: UserState::Active,
                credential: Credential::Token { repo_ids, .. },
                ..
            } => (*id, repo_ids.clone()),
            Subject::User { .. } => return Err(ServiceError::Forbidden),
            _ => return Err(ServiceError::Unauthenticated),
        };
        let ids: Option<Vec<uuid::Uuid>> =
            repos.map(|r| r.iter().map(|id| *id.as_uuid()).collect());
        let mut out = Vec::new();
        for o in slath_db::work_orders::open_for_bot(&self.pool, bot, ids.as_deref()).await? {
            let a = self
                .repo_access_by_id(subject, o.repo_id, Action::WorkOrderClaim)
                .await;
            let Ok(a) = a else { continue };
            let o = self.expire_if_due(o).await?;
            if o.state == "open" {
                out.push(self.order_view(&a, o).await?);
            }
        }
        Ok(out)
    }

    /// Ends `o` as expired if its time has passed.
    pub(crate) async fn expire_if_due(&self, o: WorkOrder) -> Result<WorkOrder> {
        if o.state == "open" && o.expires_at <= OffsetDateTime::now_utc() {
            self.end_work_order(&o, "expired", None).await?;
            return Ok(slath_db::work_orders::get_by_id(&self.pool, o.id)
                .await?
                .unwrap_or(o));
        }
        Ok(o)
    }

    /// Ends an open order with `reason`, recording the event; nothing if it
    /// has ended already. Returns whether this call ended it.
    pub(crate) async fn end_work_order(
        &self,
        o: &WorkOrder,
        reason: &str,
        by: Option<UserId>,
    ) -> Result<bool> {
        let mut tx = self.pool.begin().await.map_err(ServiceError::internal)?;
        let ended = slath_db::work_orders::end(&mut *tx, o.id, reason, by).await?;
        if ended {
            slath_db::work_orders::insert_event(
                &mut *tx,
                o.id,
                ("work_order.end", "allowed", Some(reason)),
                None,
                by,
            )
            .await?;
        }
        tx.commit().await.map_err(ServiceError::internal)?;
        Ok(ended)
    }

    /// Stops an open order: its creator, the bot's managers, and anyone
    /// with the write role. Audited.
    pub async fn stop_work_order(
        &self,
        subject: &Subject,
        (ns, repo): (&str, &str),
        number: i32,
    ) -> Result<OrderView> {
        let me = human(subject)?;
        let (a, view) = self.work_order(subject, (ns, repo), number).await?;
        let may = a.can(subject, Action::WorkOrderStop)
            || (view.order.creator_id == Some(me) && a.can(subject, Action::RepoRead))
            || self
                .managed_bot(subject, &view.bot, slath_core::authz::Scope::RepoWrite)
                .await
                .is_ok();
        if !may {
            return Err(ServiceError::Forbidden);
        }
        if view.order.state != "open" {
            return Err(ServiceError::Conflict(
                "This work order has ended already.".into(),
            ));
        }
        self.end_work_order(&view.order, "stopped", Some(me))
            .await?;
        self.audit(
            subject,
            "work_order.stop",
            Target::new("work_order", Some(view.order.id)).in_repo(*a.repo.id.as_uuid()),
            serde_json::json!({"number": number}),
        )
        .await;
        let o = slath_db::work_orders::get_by_id(&self.pool, view.order.id)
            .await?
            .ok_or(ServiceError::NotFound)?;
        self.order_view(&a, o).await
    }

    /// Exchanges the calling bot's claim credential for a task token of
    /// its order `number`, shown once. Claiming again renews: it revokes
    /// the order's earlier task tokens. Another bot gets "not found", an
    /// ended order a conflict.
    pub async fn claim_work_order(
        &self,
        subject: &Subject,
        (ns, repo): (&str, &str),
        number: i32,
    ) -> Result<Claim> {
        let bot = match subject {
            Subject::User {
                id,
                kind: UserKind::Bot,
                state: UserState::Active,
                credential: Credential::Token { .. },
                ..
            } => *id,
            Subject::User { .. } => return Err(ServiceError::Forbidden),
            _ => return Err(ServiceError::Unauthenticated),
        };
        let (a, view) = match self.work_order(subject, (ns, repo), number).await {
            Ok(x) => x,
            Err(ServiceError::Forbidden) => return Err(ServiceError::NotFound),
            Err(e) => return Err(e),
        };
        if view.order.bot_id != bot {
            return Err(ServiceError::NotFound);
        }
        if !a.can(subject, Action::WorkOrderClaim) {
            return Err(ServiceError::Forbidden);
        }
        if view.order.state != "open" {
            return Err(ServiceError::Conflict(
                "This work order has ended, so it can't be claimed.".into(),
            ));
        }
        let token = slath_auth::token::new_credential(CredentialKind::TaskToken)
            .map_err(ServiceError::internal)?;
        let minutes = i64::from(self.config.limits.task_token_minutes);
        let expires_at = (OffsetDateTime::now_utc() + time::Duration::minutes(minutes))
            .min(view.order.expires_at);
        let mut tx = self.pool.begin().await.map_err(ServiceError::internal)?;
        slath_db::work_orders::claim(
            &mut tx,
            view.order.id,
            (
                &slath_auth::token::pat_display_prefix(token.expose()),
                &slath_auth::token::storage_hash(token.expose()),
            ),
            expires_at,
        )
        .await?;
        slath_db::work_orders::insert_event(
            &mut *tx,
            view.order.id,
            ("work_order.claim", "allowed", None),
            None,
            Some(bot),
        )
        .await?;
        tx.commit().await.map_err(ServiceError::internal)?;
        self.audit(
            subject,
            "work_order.claim",
            Target::new("work_order", Some(view.order.id)).in_repo(*a.repo.id.as_uuid()),
            serde_json::json!({"number": number}),
        )
        .await;
        Ok(Claim { token, expires_at })
    }

    /// The open order of an issue or merge request, for showing it there.
    pub async fn latest_work_order(
        &self,
        a: &RepoAccess,
        issue: Option<uuid::Uuid>,
        mr: Option<uuid::Uuid>,
    ) -> Result<Option<OrderView>> {
        match slath_db::work_orders::latest_for_target(&self.pool, issue, mr).await? {
            Some(o) => {
                let o = self.expire_if_due(o).await?;
                Ok(Some(self.order_view(a, o).await?))
            }
            None => Ok(None),
        }
    }

    /// An order's events, oldest first, for its readers.
    pub async fn work_order_events(
        &self,
        subject: &Subject,
        (ns, repo): (&str, &str),
        number: i32,
        offset: i64,
        limit: i64,
    ) -> Result<Vec<(Event, Option<String>)>> {
        let (a, view) = self.work_order(subject, (ns, repo), number).await?;
        if !a.can(subject, Action::RepoRead) {
            return Err(ServiceError::Forbidden);
        }
        let events = slath_db::work_orders::events(
            &self.pool,
            view.order.id,
            offset.max(0),
            limit.clamp(1, 201),
        )
        .await?;
        let mut out = Vec::with_capacity(events.len());
        for e in events {
            let actor = match e.actor_id {
                Some(id) => slath_db::users::get(&self.pool, id).await?.map(|u| u.name),
                None => None,
            };
            out.push((e, actor));
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn orders_name_their_target_and_refuse_unknown_fields() {
        let o: NewOrder =
            serde_json::from_str(r#"{"target":{"issue":3},"bot":"b","goal":"Fix it"}"#).unwrap();
        assert_eq!(o.target, TargetRef::Issue(3));
        let m: NewOrder = serde_json::from_str(
            r#"{"target":{"merge_request":4},"bot":"b","goal":"x","scope":["src/**"]}"#,
        )
        .unwrap();
        assert_eq!(m.target, TargetRef::MergeRequest(4));
        assert!(
            serde_json::from_str::<NewOrder>(
                r#"{"target":{"issue":3},"bot":"b","goal":"x","budget":1}"#
            )
            .is_err()
        );
        assert_eq!(branch_prefix(17), "wo/17/");
    }
}
