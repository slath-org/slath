// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Webhooks: repositories and organizations send events to URLs of their
//! choice, signed with a shared secret (spec: webhooks).
//!
//! An event becomes one delivery per webhook that wants it, if the
//! webhook's principal can read the repository at that moment. Each
//! delivery is a `webhook.deliver` job; its ID stays the same across
//! retries. Every attempt checks and resolves the destination again and
//! connects only to the address it checked (no DNS rebinding), signs the
//! body with a timestamp, follows no redirects, and records the answer.
//! Failures retry with exponential backoff; a webhook that keeps failing
//! is switched off and its principal told.

use slath_core::authz::{Action, Subject, UserState};
use slath_core::ids::{OrgId, RepoId, UserId};
use slath_db::jobs::LeasedJob;
use slath_db::webhooks::{self as db, Attempt, Delivery, Owner, Webhook};
use slath_db::worker::JobError;
use time::OffsetDateTime;

use crate::Services;
use crate::error::{Result, ServiceError};
use crate::repos::RepoAccess;

/// The job that makes one attempt of a delivery.
pub const WEBHOOK_DELIVER: &str = "webhook.deliver";
/// The events a webhook can subscribe to (`ping` is sent on request).
pub const EVENTS: &[&str] = &[
    "push",
    "merge_request",
    "merge_request_comment",
    "merge_request_review",
    "fork",
    "issue",
    "issue_comment",
    "status",
    "repo",
];
/// Attempts per delivery.
pub const MAX_ATTEMPTS: i32 = 8;
/// Failed attempts in a row that switch a webhook off.
pub const AUTO_DISABLE_AFTER: i32 = 50;
/// The time an attempt may take.
const TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);
/// How much of a response is kept.
const RESPONSE_MAX: usize = 4096;
/// How long deliveries are kept, in days.
const RETENTION_DAYS: i32 = 30;
/// The longest text a payload carries in one field.
const TEXT_MAX: usize = 64 * 1024;

/// Where webhooks are managed: a repository (its administrators) or an
/// organization (its owners).
#[derive(Debug, Clone, Copy)]
pub enum WebhookOwner<'a> {
    Repo(&'a RepoAccess),
    Org(OrgId),
}

impl WebhookOwner<'_> {
    fn db(self) -> Owner {
        match self {
            Self::Repo(a) => Owner::Repo(a.repo.id),
            Self::Org(o) => Owner::Org(o),
        }
    }

    fn audit_target(self) -> crate::audit::Target {
        match self {
            Self::Repo(a) => crate::audit::Target::repo(*a.repo.id.as_uuid()),
            Self::Org(o) => crate::audit::Target::org(*o.as_uuid()),
        }
    }
}

/// What a webhook's principal must be able to read for an event to be
/// delivered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Needs {
    /// The repository.
    Repo,
    /// Its issues (and issues turned on).
    Issues,
    /// Its merge requests (and merge requests turned on).
    MergeRequests,
    /// The repository and another one (a new fork).
    Also(RepoId),
}

/// What a webhook is set to. On update, `secret: None` keeps the secret;
/// on create it means "generate one".
#[derive(Debug, Clone, Default)]
pub struct WebhookInput {
    pub url: String,
    pub events: Vec<String>,
    pub active: bool,
    pub secret: Option<String>,
}

/// A text cut to [`TEXT_MAX`] bytes, marked if it was cut.
fn text(s: &str) -> serde_json::Value {
    if s.len() <= TEXT_MAX {
        return s.into();
    }
    let mut end = TEXT_MAX;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}… (cut)", &s[..end]).into()
}

/// The wait before attempt `next` (2 and up): 1 minute, doubling, at most
/// 6 hours.
#[must_use]
pub fn backoff(next: i32) -> time::Duration {
    let minutes = 1_i64 << (next - 2).clamp(0, 20);
    time::Duration::minutes(minutes.min(6 * 60))
}

fn secret() -> Result<String> {
    use base64::Engine as _;
    let bytes = slath_auth::token::random_bytes::<24>().map_err(ServiceError::internal)?;
    Ok(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes))
}

impl Services {
    /// The user who manages webhooks of `owner`, if `subject` may.
    async fn webhook_manager(&self, subject: &Subject, owner: WebhookOwner<'_>) -> Result<UserId> {
        match owner {
            WebhookOwner::Repo(a) => {
                self.authorize(subject, a, Action::WebhooksManage)?;
                match subject {
                    Subject::User { id, .. } => Ok(*id),
                    _ => Err(ServiceError::Forbidden),
                }
            }
            // Others can't tell an organization's webhooks exist.
            WebhookOwner::Org(o) => match self.require_org_owner(subject, o).await {
                Err(ServiceError::Forbidden) => Err(ServiceError::NotFound),
                other => other,
            },
        }
    }

    /// Checks a URL, events, and secret; returns the URL as stored.
    async fn check_webhook(&self, input: &WebhookInput) -> Result<String> {
        let cfg = &self.config.webhooks;
        let mut errors = Vec::new();
        let url = match crate::net::parse_keeping_query(input.url.trim(), cfg.allow_http) {
            Ok(u) => match crate::net::resolve(&u, &cfg.allow_networks, &self.own_host()).await {
                Ok(_) => Some(u.to_string()),
                Err(e) => {
                    errors.push(crate::FieldError::new("url", "unreachable", e.to_string()));
                    None
                }
            },
            Err(crate::net::UrlError::Scheme) if cfg.allow_http => {
                errors.push(crate::FieldError::new(
                    "url",
                    "scheme",
                    "Enter an https:// or http:// URL.",
                ));
                None
            }
            Err(e) => {
                errors.push(crate::FieldError::new("url", "invalid", e.to_string()));
                None
            }
        };
        if input.events.is_empty() {
            errors.push(crate::FieldError::new(
                "events",
                "required",
                "Choose at least one event.",
            ));
        } else if let Some(bad) = input.events.iter().find(|e| !EVENTS.contains(&e.as_str())) {
            errors.push(crate::FieldError::new(
                "events",
                "invalid",
                format!("There's no event named {bad}."),
            ));
        }
        if let Some(s) = &input.secret
            && !(16..=256).contains(&s.chars().count())
        {
            errors.push(crate::FieldError::new(
                "secret",
                "length",
                "Use a secret of 16 to 256 characters, or leave it empty to have one generated.",
            ));
        }
        match url {
            Some(u) if errors.is_empty() => Ok(u),
            _ => Err(ServiceError::Invalid(errors)),
        }
    }

    fn events_of(input: &WebhookInput) -> Vec<String> {
        let mut events: Vec<String> = EVENTS
            .iter()
            .filter(|e| input.events.iter().any(|x| x == *e))
            .map(|e| (*e).to_owned())
            .collect();
        events.dedup();
        events
    }

    /// The webhooks of `owner`.
    pub async fn webhooks(
        &self,
        subject: &Subject,
        owner: WebhookOwner<'_>,
    ) -> Result<Vec<Webhook>> {
        self.webhook_manager(subject, owner).await?;
        Ok(db::list(&self.pool, owner.db()).await?)
    }

    /// One webhook of `owner` (not found if it belongs elsewhere).
    pub async fn webhook(
        &self,
        subject: &Subject,
        owner: WebhookOwner<'_>,
        id: uuid::Uuid,
    ) -> Result<Webhook> {
        self.webhook_manager(subject, owner).await?;
        self.owned_webhook(owner, id).await
    }

    async fn owned_webhook(&self, owner: WebhookOwner<'_>, id: uuid::Uuid) -> Result<Webhook> {
        let w = db::get(&self.pool, id)
            .await?
            .ok_or(ServiceError::NotFound)?;
        let mine = match owner.db() {
            Owner::Repo(r) => w.repo_id == Some(r),
            Owner::Org(o) => w.org_id == Some(o),
        };
        if mine {
            Ok(w)
        } else {
            Err(ServiceError::NotFound)
        }
    }

    /// Creates a webhook; returns it and the secret if Slath generated it
    /// (shown once). Audited.
    pub async fn create_webhook(
        &self,
        subject: &Subject,
        owner: WebhookOwner<'_>,
        input: &WebhookInput,
    ) -> Result<(Webhook, Option<String>)> {
        let me = self.webhook_manager(subject, owner).await?;
        let url = self.check_webhook(input).await?;
        if db::list(&self.pool, owner.db()).await?.len() >= self.config.limits.webhooks_per_owner {
            return Err(ServiceError::invalid(
                "url",
                "limit",
                format!(
                    "There can be at most {} webhooks here (limits.webhooks_per_owner).",
                    self.config.limits.webhooks_per_owner
                ),
            ));
        }
        let generated = match &input.secret {
            Some(_) => None,
            None => Some(secret()?),
        };
        let plain = input
            .secret
            .clone()
            .or_else(|| generated.clone())
            .unwrap_or_default();
        let id = uuid::Uuid::now_v7();
        let sealed = self
            .seal(plain.as_bytes(), "webhooks", "secret_enc", &id.to_string())
            .await?;
        let events = Self::events_of(input);
        db::insert(
            &self.pool,
            id,
            owner.db(),
            &db::Fields {
                url: &url,
                events: &events,
                active: input.active,
                secret: Some((&sealed.ciphertext, &sealed.key_id)),
                principal: me,
            },
        )
        .await?;
        self.audit(
            subject,
            "webhook.create",
            owner.audit_target(),
            serde_json::json!({"webhook": id, "url": url, "events": events}),
        )
        .await;
        let w = db::get(&self.pool, id)
            .await?
            .ok_or(ServiceError::NotFound)?;
        Ok((w, generated))
    }

    /// Changes a webhook; the one who changes it becomes its principal.
    /// Audited.
    pub async fn update_webhook(
        &self,
        subject: &Subject,
        owner: WebhookOwner<'_>,
        id: uuid::Uuid,
        input: &WebhookInput,
    ) -> Result<Webhook> {
        let me = self.webhook_manager(subject, owner).await?;
        self.owned_webhook(owner, id).await?;
        let url = self.check_webhook(input).await?;
        let sealed = match &input.secret {
            Some(s) => Some(
                self.seal(s.as_bytes(), "webhooks", "secret_enc", &id.to_string())
                    .await?,
            ),
            None => None,
        };
        let events = Self::events_of(input);
        db::update(
            &self.pool,
            id,
            &db::Fields {
                url: &url,
                events: &events,
                active: input.active,
                secret: sealed
                    .as_ref()
                    .map(|s| (s.ciphertext.as_slice(), s.key_id.as_str())),
                principal: me,
            },
        )
        .await?;
        self.audit(
            subject,
            "webhook.update",
            owner.audit_target(),
            serde_json::json!({
                "webhook": id, "url": url, "events": events, "active": input.active,
                "secret_changed": sealed.is_some(),
            }),
        )
        .await;
        db::get(&self.pool, id).await?.ok_or(ServiceError::NotFound)
    }

    /// Deletes a webhook and its deliveries. Audited.
    pub async fn delete_webhook(
        &self,
        subject: &Subject,
        owner: WebhookOwner<'_>,
        id: uuid::Uuid,
    ) -> Result<()> {
        self.webhook_manager(subject, owner).await?;
        let w = self.owned_webhook(owner, id).await?;
        db::delete(&self.pool, id).await?;
        self.audit(
            subject,
            "webhook.delete",
            owner.audit_target(),
            serde_json::json!({"webhook": id, "url": w.url}),
        )
        .await;
        Ok(())
    }

    /// A webhook's deliveries, newest first.
    pub async fn webhook_deliveries(
        &self,
        subject: &Subject,
        owner: WebhookOwner<'_>,
        id: uuid::Uuid,
        limit: i64,
    ) -> Result<Vec<Delivery>> {
        self.webhook(subject, owner, id).await?;
        Ok(db::deliveries(&self.pool, id, limit.clamp(1, 100)).await?)
    }

    /// One delivery of a webhook, with its attempts.
    pub async fn webhook_delivery(
        &self,
        subject: &Subject,
        owner: WebhookOwner<'_>,
        (id, delivery): (uuid::Uuid, uuid::Uuid),
    ) -> Result<(Delivery, Vec<Attempt>)> {
        self.webhook(subject, owner, id).await?;
        let d = db::delivery(&self.pool, delivery)
            .await?
            .filter(|d| d.webhook_id == id)
            .ok_or(ServiceError::NotFound)?;
        let attempts = db::attempts(&self.pool, delivery).await?;
        Ok((d, attempts))
    }

    /// Sends a delivery's payload again as a new delivery; returns its ID.
    pub async fn redeliver_webhook(
        &self,
        subject: &Subject,
        owner: WebhookOwner<'_>,
        (id, delivery): (uuid::Uuid, uuid::Uuid),
    ) -> Result<uuid::Uuid> {
        let (d, _) = self
            .webhook_delivery(subject, owner, (id, delivery))
            .await?;
        self.queue_delivery(id, (&d.event, d.action.as_deref()), &d.payload, Some(d.id))
            .await
    }

    /// Sends a `ping` to a webhook; returns the delivery's ID.
    pub async fn ping_webhook(
        &self,
        subject: &Subject,
        owner: WebhookOwner<'_>,
        id: uuid::Uuid,
    ) -> Result<uuid::Uuid> {
        let w = self.webhook(subject, owner, id).await?;
        let payload = serde_json::json!({
            "event": "ping",
            "zen": "Small changes, reviewed well.",
            "webhook": {"id": w.id, "events": w.events},
        });
        self.queue_delivery(id, ("ping", None), &payload, None)
            .await
    }

    async fn queue_delivery(
        &self,
        webhook: uuid::Uuid,
        event: (&str, Option<&str>),
        payload: &serde_json::Value,
        redelivery_of: Option<uuid::Uuid>,
    ) -> Result<uuid::Uuid> {
        let id = uuid::Uuid::now_v7();
        db::insert_delivery(&self.pool, id, webhook, event, payload, redelivery_of).await?;
        self.queue_attempt(id, None).await?;
        Ok(id)
    }

    async fn queue_attempt(&self, delivery: uuid::Uuid, at: Option<OffsetDateTime>) -> Result<()> {
        let mut job = slath_db::jobs::NewJob::new(
            "default",
            WEBHOOK_DELIVER,
            serde_json::json!({"delivery_id": delivery.to_string()}),
        );
        job.run_at = at;
        job.max_attempts = 1;
        slath_db::jobs::enqueue(&self.pool, &job).await?;
        Ok(())
    }

    /// Queues `event` for every webhook of `repo` (and of its
    /// organization) that wants it and whose principal can read what
    /// `needs` says right now. Never fails the caller: problems are logged.
    pub(crate) async fn emit_webhook(
        &self,
        repo: RepoId,
        actor: Option<UserId>,
        (event, action): (&str, Option<&str>),
        needs: Needs,
        object: serde_json::Value,
    ) {
        if let Err(e) = self
            .emit_inner(repo, actor, (event, action), needs, object)
            .await
        {
            tracing::warn!(error = %e, repo = %repo, event, "queueing webhook deliveries failed");
        }
    }

    async fn emit_inner(
        &self,
        repo: RepoId,
        actor: Option<UserId>,
        (event, action): (&str, Option<&str>),
        needs: Needs,
        object: serde_json::Value,
    ) -> Result<()> {
        let hooks: Vec<Webhook> = db::for_repo(&self.pool, repo)
            .await?
            .into_iter()
            .filter(|w| w.events.iter().any(|e| e == event))
            .collect();
        if hooks.is_empty() {
            return Ok(());
        }
        let Some(r) = slath_db::repos::get(&self.pool, repo).await? else {
            return Ok(());
        };
        let sender = match actor {
            Some(u) => slath_db::users::get(&self.pool, u).await?.map(|u| {
                serde_json::json!({
                    "username": u.name,
                    "kind": if u.kind == slath_core::authz::UserKind::Bot { "bot" } else { "user" },
                })
            }),
            None => None,
        };
        let base = self.web_base();
        let mut payload = serde_json::json!({
            "event": event,
            "repository": {
                "id": r.id.to_string(),
                "full_name": format!("{}/{}", r.namespace, r.name),
                "visibility": r.visibility,
                "html_url": format!("{base}/{}/{}", r.namespace, r.name),
                "api_url": format!("{base}/api/v1/repos/{}/{}", r.namespace, r.name),
            },
            "sender": sender,
        });
        if let Some(a) = action {
            payload["action"] = a.into();
        }
        if let (serde_json::Value::Object(p), serde_json::Value::Object(o)) = (&mut payload, object)
        {
            p.extend(o);
        }
        for w in hooks {
            let Some(principal) = w.principal_id else {
                continue;
            };
            let readable = match needs {
                Needs::Repo => self.user_can_read_repo(principal, repo).await,
                Needs::Issues => self.user_can_read(principal, repo, "issue").await,
                Needs::MergeRequests => self.user_can_read(principal, repo, "merge_request").await,
                Needs::Also(other) => {
                    self.user_can_read_repo(principal, repo).await
                        && self.user_can_read_repo(principal, other).await
                }
            };
            if !readable {
                continue;
            }
            self.queue_delivery(w.id, (event, action), &payload, None)
                .await?;
        }
        Ok(())
    }

    async fn user_can_read_repo(&self, user: UserId, repo: RepoId) -> bool {
        let Ok(Some(row)) = slath_db::users::get(&self.pool, user).await else {
            return false;
        };
        let them = crate::notifications::user_subject(user, &row);
        self.repo_access_by_id(&them, repo, Action::RepoRead)
            .await
            .is_ok()
    }

    /// The `webhook.deliver` job: one attempt of a delivery.
    pub(crate) async fn run_webhook_delivery(
        &self,
        job: &LeasedJob,
    ) -> std::result::Result<(), JobError> {
        let id: uuid::Uuid = job
            .payload
            .get("delivery_id")
            .and_then(|v| v.as_str())
            .and_then(|s| s.parse().ok())
            .ok_or_else(|| JobError::Permanent("payload has no delivery_id".to_owned()))?;
        let temp = |e: slath_db::DbError| JobError::Retry(e.to_string());
        let Some(d) = db::delivery(&self.pool, id).await.map_err(temp)? else {
            return Ok(());
        };
        if d.state != "pending" {
            return Ok(());
        }
        let Some(w) = db::get(&self.pool, d.webhook_id).await.map_err(temp)? else {
            return Ok(());
        };
        // Pings and redeliveries go out even to a switched-off webhook only
        // if someone turned it on again; otherwise the delivery ends here.
        if !w.active {
            db::give_up(&self.pool, id).await.map_err(temp)?;
            return Ok(());
        }
        if !self
            .principal_active(&w)
            .await
            .map_err(|e| JobError::Retry(e.to_string()))?
        {
            return Ok(());
        }
        let attempt = d.attempts + 1;
        let started = OffsetDateTime::now_utc();
        let clock = std::time::Instant::now();
        let result = self.attempt(&w, &d).await;
        let duration_ms = i32::try_from(clock.elapsed().as_millis()).unwrap_or(i32::MAX);
        let (status, response, error) = match &result {
            Ok(r) => (
                Some(i32::from(r.status)),
                Some(
                    String::from_utf8_lossy(&r.body)
                        .chars()
                        .take(RESPONSE_MAX)
                        .collect::<String>(),
                ),
                (!(200..300).contains(&r.status)).then(|| match r.status {
                    300..=399 => "redirects aren't followed".to_owned(),
                    s => format!("the receiver answered {s}"),
                }),
            ),
            Err(e) => (None, None, Some(e.chars().take(500).collect())),
        };
        let ok = error.is_none();
        let (state, next) = if ok {
            ("delivered", None)
        } else if attempt < MAX_ATTEMPTS {
            (
                "pending",
                Some(OffsetDateTime::now_utc() + backoff(attempt + 1)),
            )
        } else {
            ("failed", None)
        };
        let record = Attempt {
            attempt,
            started_at: started,
            duration_ms,
            status,
            response: response.map(|r| {
                let mut r = r;
                while r.len() > RESPONSE_MAX {
                    r.pop();
                }
                r
            }),
            error,
        };
        db::record_attempt(&self.pool, id, &record, (state, next))
            .await
            .map_err(temp)?;
        if ok {
            db::reset_failures(&self.pool, w.id).await.map_err(temp)?;
        } else {
            let n = db::add_failure(&self.pool, w.id).await.map_err(temp)?;
            if n >= AUTO_DISABLE_AFTER {
                self.switch_off(&w, &format!("{n} failed deliveries in a row"))
                    .await;
            } else if state == "pending" {
                self.queue_attempt(id, next)
                    .await
                    .map_err(|e| JobError::Retry(e.to_string()))?;
            }
        }
        Ok(())
    }

    /// Whether the webhook's principal can still act; if not, the webhook
    /// is switched off.
    async fn principal_active(&self, w: &Webhook) -> Result<bool> {
        let user = match w.principal_id {
            Some(p) => slath_db::users::get(&self.pool, p).await?,
            None => None,
        };
        if user.as_ref().is_some_and(|u| u.state == UserState::Active) {
            return Ok(true);
        }
        self.switch_off(w, "its principal's account is gone or suspended")
            .await;
        Ok(false)
    }

    /// One attempt: checks and resolves the destination, signs, and sends.
    async fn attempt(
        &self,
        w: &Webhook,
        d: &Delivery,
    ) -> std::result::Result<crate::net::Response, String> {
        let cfg = &self.config.webhooks;
        let url =
            crate::net::parse_keeping_query(&w.url, cfg.allow_http).map_err(|e| e.to_string())?;
        let dest = crate::net::resolve(&url, &cfg.allow_networks, &self.own_host())
            .await
            .map_err(|e| e.to_string())?;
        let secret = self
            .open(
                &w.secret_enc,
                &w.key_id,
                "webhooks",
                "secret_enc",
                &w.id.to_string(),
            )
            .map_err(|_| "the webhook's secret can't be read".to_owned())?;
        let body = serde_json::to_vec(&d.payload).map_err(|e| e.to_string())?;
        let ts = OffsetDateTime::now_utc().unix_timestamp();
        let signature = slath_auth::webhook::signature(secret.expose(), ts, &body);
        let headers = [
            ("X-Slath-Event", d.event.clone()),
            ("X-Slath-Delivery", d.id.to_string()),
            ("X-Slath-Timestamp", ts.to_string()),
            ("X-Slath-Signature-256", signature),
        ];
        crate::net::post(&dest, &headers, body, TIMEOUT, RESPONSE_MAX).await
    }

    /// Switches a webhook off, tells its principal by email, and audits it.
    async fn switch_off(&self, w: &Webhook, why: &str) {
        match db::disable(&self.pool, w.id, why).await {
            Ok(true) => {}
            Ok(false) => return,
            Err(e) => {
                tracing::warn!(error = %e, webhook = %w.id, "switching a webhook off failed");
                return;
            }
        }
        let target = match (w.repo_id, w.org_id) {
            (Some(r), _) => crate::audit::Target::repo(*r.as_uuid()),
            (_, Some(o)) => crate::audit::Target::org(*o.as_uuid()),
            _ => crate::audit::Target::new("webhook", Some(w.id)),
        };
        self.audit(
            &Subject::System,
            "webhook.disabled",
            target,
            serde_json::json!({"webhook": w.id, "url": w.url, "reason": why}),
        )
        .await;
        let Some(p) = w.principal_id else { return };
        let Ok(Some(u)) = slath_db::users::get(&self.pool, p).await else {
            return;
        };
        let Some(to) = u.email else { return };
        let email = crate::email::Email {
            to,
            subject: "A webhook was switched off".to_owned(),
            text: format!(
                "Slath switched off the webhook to {} ({why}).\n\n\
                 Check the receiver, then turn the webhook on again in its settings; \
                 the delivery log shows what went wrong.\n",
                w.url
            ),
            ..crate::email::Email::default()
        };
        if let Err(e) = self.queue_email(&email).await {
            tracing::warn!(error = %e, "queueing the webhook email failed");
        }
    }

    /// Deletes deliveries older than 30 days. Run periodically.
    pub async fn purge_webhook_deliveries(&self) -> Result<u64> {
        Ok(db::purge(&self.pool, RETENTION_DAYS).await?)
    }
}

impl Services {
    fn web_base(&self) -> String {
        self.config
            .server
            .public_url
            .origin()
            .trim_end_matches('/')
            .to_owned()
    }

    /// Webhook deliveries for a change to a repository itself.
    pub(crate) async fn webhook_repo(
        &self,
        subject: &Subject,
        repo: RepoId,
        action: &str,
        change: serde_json::Value,
    ) {
        let actor = match subject {
            Subject::User { id, .. } => Some(*id),
            _ => None,
        };
        self.emit_webhook(
            repo,
            actor,
            ("repo", Some(action)),
            Needs::Repo,
            serde_json::json!({"change": change}),
        )
        .await;
    }

    /// Webhook deliveries for a push: one `push` event per updated ref.
    pub(crate) async fn webhook_push(
        &self,
        repo: RepoId,
        pusher: Option<UserId>,
        updates: &[slath_git::ipc::RefUpdate],
    ) {
        const ZERO: &str = "0000000000000000000000000000000000000000";
        if db::for_repo(&self.pool, repo).await.map_or(true, |hs| {
            !hs.iter().any(|w| w.events.iter().any(|e| e == "push"))
        }) {
            return;
        }
        let dir = self.repo_path(repo);
        for u in updates {
            let old = (u.old != ZERO)
                .then(|| slath_git::ObjectId::parse(&u.old).ok())
                .flatten();
            let new = (u.new != ZERO)
                .then(|| slath_git::ObjectId::parse(&u.new).ok())
                .flatten();
            let (commits, forced) = match (&old, &new) {
                (_, Some(n)) => {
                    let log = slath_git::browse::log_range(&self.runner, &dir, old.as_ref(), n, 20)
                        .await
                        .unwrap_or_default();
                    let forced = match &old {
                        Some(o) => !slath_git::merge::is_ancestor(&self.runner, &dir, o, n)
                            .await
                            .unwrap_or(true),
                        None => false,
                    };
                    (log, forced)
                }
                (_, None) => (Vec::new(), false),
            };
            let commits: Vec<serde_json::Value> = commits
                .iter()
                .map(|c| {
                    serde_json::json!({
                        "id": c.oid.to_string(),
                        "message": text(&c.subject),
                        "author": {"name": c.author_name, "email": c.author_email},
                        "timestamp": c.author_time,
                    })
                })
                .collect();
            let action = match (&old, &new) {
                (None, _) => "created",
                (_, None) => "deleted",
                _ => "updated",
            };
            let payload = serde_json::json!({
                "push": {
                    "ref": String::from_utf8_lossy(&u.name),
                    "before": u.old,
                    "after": u.new,
                    "forced": forced,
                    "commits": commits,
                },
            });
            self.emit_webhook(repo, pusher, ("push", Some(action)), Needs::Repo, payload)
                .await;
        }
    }

    /// Webhook deliveries for an action on an issue.
    pub(crate) async fn webhook_issue(
        &self,
        actor: Option<UserId>,
        a: &RepoAccess,
        issue: &slath_db::issues::Issue,
        what: &crate::notifications::Activity,
    ) {
        use crate::notifications::Activity as A;
        let (ns, name) = (&a.repo.namespace, &a.repo.name);
        let base = self.web_base();
        let object = serde_json::json!({
            "number": issue.number,
            "title": text(&issue.title),
            "body": text(&issue.body),
            "state": issue.state,
            "author": issue.author,
            "html_url": format!("{base}/{ns}/{name}/issues/{}", issue.number),
            "api_url": format!("{base}/api/v1/repos/{ns}/{name}/issues/{}", issue.number),
        });
        let (event, action, extra) = match what {
            A::Opened => ("issue", "opened", None),
            A::Closed => ("issue", "closed", None),
            A::Reopened => ("issue", "reopened", None),
            A::Assigned(_) => ("issue", "assigned", None),
            A::Commented(c) => ("issue_comment", "created", self.comment_object(*c).await),
        };
        let mut payload = serde_json::json!({"issue": object});
        if let Some(c) = extra {
            payload["comment"] = c;
        }
        self.emit_webhook(
            a.repo.id,
            actor,
            (event, Some(action)),
            Needs::Issues,
            payload,
        )
        .await;
    }

    async fn comment_object(&self, id: uuid::Uuid) -> Option<serde_json::Value> {
        let c = slath_db::issues::comment(&self.pool, id)
            .await
            .ok()
            .flatten()?;
        Some(serde_json::json!({
            "id": c.id,
            "body": text(&c.body),
            "author": c.author,
            "created_at": c.created_at.format(&time::format_description::well_known::Rfc3339).ok(),
        }))
    }

    /// Webhook deliveries for an action on a merge request.
    pub(crate) async fn webhook_mr(
        &self,
        actor: Option<UserId>,
        m: &slath_db::merge_requests::MergeRequest,
        what: &crate::notifications::MrActivity,
    ) {
        use crate::notifications::MrActivity as A;
        let Ok(Some(r)) = slath_db::repos::get(&self.pool, m.repo_id).await else {
            return;
        };
        let (ns, name) = (&r.namespace, &r.name);
        let base = self.web_base();
        let object = serde_json::json!({
            "number": m.number,
            "title": text(&m.title),
            "description": text(&m.description),
            "state": m.state,
            "draft": m.draft,
            "author": m.author,
            "source_branch": m.source_branch(),
            "target_branch": m.target_branch,
            "head_oid": m.head_oid,
            "revisions": m.revisions,
            "html_url": format!("{base}/{ns}/{name}/merge-requests/{}", m.number),
            "api_url": format!("{base}/api/v1/repos/{ns}/{name}/merge-requests/{}", m.number),
        });
        let mut payload = serde_json::json!({"merge_request": object});
        let (event, action) = match what {
            A::Opened => ("merge_request", "opened"),
            A::Closed => ("merge_request", "closed"),
            A::Reopened => ("merge_request", "reopened"),
            A::Merged => ("merge_request", "merged"),
            A::Ready => ("merge_request", "ready"),
            A::Revision(_) => ("merge_request", "updated"),
            A::Commented(c) => {
                if let Some(c) = self.comment_object(*c).await {
                    payload["comment"] = c;
                }
                ("merge_request_comment", "created")
            }
            A::ThreadComment(c) => {
                let found = slath_db::mr_threads::comments(&self.pool, m.id)
                    .await
                    .ok()
                    .and_then(|cs| cs.into_iter().find(|x| x.id == *c));
                if let Some(c) = found {
                    payload["comment"] = serde_json::json!({
                        "id": c.id,
                        "thread_id": c.thread_id,
                        "body": text(&c.body),
                        "author": c.author,
                    });
                }
                ("merge_request_comment", "created")
            }
            A::Reviewed(id) => {
                let found = slath_db::mr_reviews::list(&self.pool, m.id)
                    .await
                    .ok()
                    .and_then(|rs| rs.into_iter().find(|x| x.id == *id));
                if let Some(v) = found {
                    payload["review"] = serde_json::json!({
                        "id": v.id,
                        "verdict": v.verdict,
                        "body": text(&v.body),
                        "revision": v.revision_seq,
                        "reviewer": v.reviewer,
                    });
                }
                ("merge_request_review", "submitted")
            }
            _ => return,
        };
        self.emit_webhook(
            m.repo_id,
            actor,
            (event, Some(action)),
            Needs::MergeRequests,
            payload,
        )
        .await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_doubles_up_to_six_hours() {
        let mins: Vec<i64> = (2..=8).map(|n| backoff(n).whole_minutes()).collect();
        assert_eq!(mins, [1, 2, 4, 8, 16, 32, 64]);
        assert_eq!(backoff(20).whole_minutes(), 360);
    }

    #[test]
    fn long_texts_are_cut_and_marked() {
        let long = "é".repeat(TEXT_MAX);
        let v = text(&long);
        let s = v.as_str().unwrap();
        assert!(s.ends_with("… (cut)") && s.len() < long.len());
        assert_eq!(text("short"), "short");
    }
}
