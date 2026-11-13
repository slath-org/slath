// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Stopping and re-enabling bots: a stop suspends the bot and removes its
//! tokens and SSH keys at once, so nothing it holds keeps working; only a
//! stopped bot can be re-enabled, and then without credentials
//! (spec/04 bot accounts).

use slath_core::authz::{Credential, Scope, Subject, UserKind, UserState};
use slath_core::ids::UserId;
use slath_db::bots::Bot;

use crate::Services;
use crate::audit::Target;
use crate::error::{Result, ServiceError};

/// What a stop removed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Stopped {
    /// False if it was stopped already (nothing changed).
    pub changed: bool,
    pub tokens_revoked: u64,
    pub keys_removed: u64,
}

/// A site administrator acting through a session or an `admin` token.
fn site_admin(subject: &Subject) -> bool {
    matches!(
        subject,
        Subject::User {
            is_site_admin: true,
            state: UserState::Active,
            kind: UserKind::Human,
            credential: Credential::Session | Credential::Token { .. },
            ..
        }
    )
}

impl Services {
    /// A bot the subject may stop and re-enable: its managers, and site
    /// administrators for every bot. Everyone else gets "not found".
    pub async fn controlled_bot(&self, subject: &Subject, name: &str) -> Result<Bot> {
        self.overseen_bot(subject, name, Scope::UserWrite).await
    }

    async fn overseen_bot(&self, subject: &Subject, name: &str, scope: Scope) -> Result<Bot> {
        match self.managed_bot(subject, name, scope).await {
            Ok(b) => Ok(b),
            Err(ServiceError::NotFound) if site_admin(subject) => {
                slath_db::bots::find(&self.pool, name.trim())
                    .await?
                    .filter(|b| b.state != UserState::Deleted)
                    .ok_or(ServiceError::NotFound)
            }
            Err(e) => Err(e),
        }
    }

    /// Stops a bot (UI, API).
    pub async fn stop_bot(&self, subject: &Subject, name: &str) -> Result<Stopped> {
        let bot = self.controlled_bot(subject, name).await?;
        let by = match subject {
            Subject::User { id, .. } => Some(*id),
            _ => None,
        };
        self.stop(subject, &bot, by).await
    }

    /// Re-enables a stopped bot (UI, API). A bot suspended any other way
    /// stays as it is.
    pub async fn start_bot(&self, subject: &Subject, name: &str) -> Result<bool> {
        let bot = self.controlled_bot(subject, name).await?;
        self.start(subject, &bot).await
    }

    /// `slath admin bot stop`.
    pub async fn stop_bot_as_system(&self, name: &str) -> Result<Stopped> {
        let bot = self.system_bot(name).await?;
        self.stop(&Subject::System, &bot, None).await
    }

    /// `slath admin bot start`.
    pub async fn start_bot_as_system(&self, name: &str) -> Result<bool> {
        let bot = self.system_bot(name).await?;
        self.start(&Subject::System, &bot).await
    }

    async fn system_bot(&self, name: &str) -> Result<Bot> {
        slath_db::bots::find(&self.pool, name.trim())
            .await?
            .filter(|b| b.state != UserState::Deleted)
            .ok_or_else(|| {
                ServiceError::invalid("name", "unknown", "There's no bot with this name.")
            })
    }

    async fn stop(&self, subject: &Subject, bot: &Bot, by: Option<UserId>) -> Result<Stopped> {
        let mut tx = self.pool.begin().await.map_err(ServiceError::internal)?;
        let done = slath_db::bots::stop(&mut tx, bot.id, by).await?;
        tx.commit().await.map_err(ServiceError::internal)?;
        let Some((tokens, keys)) = done else {
            return Ok(Stopped::default());
        };
        self.audit(
            subject,
            "bot.stop",
            Target::new("user", Some(*bot.id.as_uuid())),
            serde_json::json!({"bot": bot.name, "tokens_revoked": tokens, "keys_removed": keys}),
        )
        .await;
        Ok(Stopped {
            changed: true,
            tokens_revoked: tokens,
            keys_removed: keys,
        })
    }

    async fn start(&self, subject: &Subject, bot: &Bot) -> Result<bool> {
        if bot.stopped_at.is_none() {
            if bot.state == UserState::Active {
                return Ok(false);
            }
            return Err(ServiceError::Conflict(
                "This bot was suspended, not stopped; a site administrator can reactivate it."
                    .to_owned(),
            ));
        }
        // A bot can't be active while the person who owns it isn't.
        if let Some(owner) = slath_db::users::bot_owner_user(&self.pool, bot.id).await?
            && slath_db::users::get(&self.pool, owner)
                .await?
                .is_none_or(|o| o.state != UserState::Active)
        {
            return Err(ServiceError::Conflict(
                "This bot's owner is suspended; reactivate the owner first.".to_owned(),
            ));
        }
        if !slath_db::bots::start(&self.pool, bot.id).await? {
            return Ok(false);
        }
        self.audit(
            subject,
            "bot.start",
            Target::new("user", Some(*bot.id.as_uuid())),
            serde_json::json!({"bot": bot.name}),
        )
        .await;
        Ok(true)
    }

    /// Why a bot's push is refused now, if it is: pre-receive re-reads the
    /// pusher, so a bot stopped or frozen during its push can't finish it.
    pub(crate) async fn bot_push_refusal(&self, subject: &Subject) -> Result<Option<&'static str>> {
        let Subject::User {
            id,
            kind: UserKind::Bot,
            ..
        } = subject
        else {
            return Ok(None);
        };
        let state = slath_db::users::get(&self.pool, *id)
            .await?
            .map(|u| u.state);
        if state != Some(UserState::Active) {
            return Ok(Some(
                "slath: push rejected: this bot is stopped or suspended",
            ));
        }
        if slath_db::bots::frozen(&self.pool, *id).await? {
            return Ok(Some("slath: push rejected: bots are frozen here"));
        }
        Ok(None)
    }
}

/// What a freeze covers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FreezeScope {
    /// Every bot of the instance (site administrators).
    Instance,
    /// The bots an organization owns (its owners and site administrators).
    Org(slath_core::ids::OrgId),
}

impl FreezeScope {
    fn org(self) -> Option<slath_core::ids::OrgId> {
        match self {
            Self::Instance => None,
            Self::Org(o) => Some(o),
        }
    }
}

impl Services {
    /// Only those who may freeze a scope may see or change it.
    async fn may_freeze(&self, subject: &Subject, scope: FreezeScope) -> Result<()> {
        match scope {
            FreezeScope::Instance if site_admin(subject) => Ok(()),
            FreezeScope::Instance => Err(ServiceError::NotFound),
            FreezeScope::Org(org) => self.require_org_owner(subject, org).await.map(|_| ()),
        }
    }

    /// The freeze of a scope, if there is one.
    pub async fn bot_freeze(
        &self,
        subject: &Subject,
        scope: FreezeScope,
    ) -> Result<Option<slath_db::bots::Freeze>> {
        self.may_freeze(subject, scope).await?;
        Ok(slath_db::bots::freeze(&self.pool, scope.org()).await?)
    }

    /// Freezes (`true`) or unfreezes a scope; returns whether it changed.
    pub async fn set_bot_freeze(
        &self,
        subject: &Subject,
        scope: FreezeScope,
        frozen: bool,
    ) -> Result<bool> {
        self.may_freeze(subject, scope).await?;
        let by = match subject {
            Subject::User { id, .. } => Some(*id),
            _ => None,
        };
        self.freeze(subject, scope, frozen, by).await
    }

    /// `slath admin bot freeze|unfreeze`.
    pub async fn set_bot_freeze_as_system(&self, org: Option<&str>, frozen: bool) -> Result<bool> {
        let scope = match org {
            None => FreezeScope::Instance,
            Some(name) => match self.resolve_namespace(name.trim()).await? {
                Some((_, slath_db::orgs::NamespaceKind::Org(o))) => FreezeScope::Org(o),
                _ => {
                    return Err(ServiceError::invalid(
                        "org",
                        "unknown",
                        "There's no organization with this name.",
                    ));
                }
            },
        };
        self.freeze(&Subject::System, scope, frozen, None).await
    }

    async fn freeze(
        &self,
        subject: &Subject,
        scope: FreezeScope,
        frozen: bool,
        by: Option<UserId>,
    ) -> Result<bool> {
        let changed = if frozen {
            slath_db::bots::set_freeze(&self.pool, scope.org(), by).await?
        } else {
            slath_db::bots::clear_freeze(&self.pool, scope.org()).await?
        };
        if changed {
            let target = match scope.org() {
                Some(o) => Target::org(*o.as_uuid()),
                None => Target::new("instance", None),
            };
            self.audit(
                subject,
                if frozen { "bot.freeze" } else { "bot.unfreeze" },
                target,
                serde_json::json!({"scope": if scope.org().is_some() { "org" } else { "instance" }}),
            )
            .await;
        }
        Ok(changed)
    }

    /// True if a freeze applies to this bot; never for people.
    pub(crate) async fn bot_frozen(&self, user: UserId, kind: UserKind) -> Result<bool> {
        if kind != UserKind::Bot {
            return Ok(false);
        }
        Ok(slath_db::bots::frozen(&self.pool, user).await?)
    }
}

/// What a bot's managers see of it (spec/04 bot accounts, overview).
#[derive(Debug, Clone)]
pub struct BotOverview {
    pub bot: Bot,
    /// A freeze applies to it.
    pub frozen: bool,
    /// Open merge requests it opened, in repositories the viewer can read.
    pub open_mr_count: i64,
    /// The newest of them (at most [`OVERVIEW_ITEMS`]).
    pub open_mrs: Vec<slath_db::bots::OpenMr>,
    /// Its newest pushes (at most [`OVERVIEW_ITEMS`]).
    pub pushes: Vec<slath_db::bots::RecentPush>,
    /// Its usable token that expires next: (name, expiry).
    pub next_expiry: Option<(String, time::OffsetDateTime)>,
}

/// How many merge requests and pushes an overview lists.
pub const OVERVIEW_ITEMS: i64 = 5;

impl Services {
    /// One bot's overview, for its managers and site administrators.
    pub async fn bot_overview(&self, subject: &Subject, name: &str) -> Result<BotOverview> {
        let bot = self.overseen_bot(subject, name, Scope::UserRead).await?;
        self.overview_of(subject, bot).await
    }

    /// The overviews of an owner's bots (as [`Services::list_bots`] lists
    /// them).
    pub async fn bot_overviews(
        &self,
        subject: &Subject,
        owner: crate::bots::Owner<'_>,
    ) -> Result<Vec<BotOverview>> {
        let mut out = Vec::new();
        for bot in self.list_bots(subject, owner).await? {
            out.push(self.overview_of(subject, bot).await?);
        }
        Ok(out)
    }

    /// Every bot of the instance, for site administrators.
    pub async fn all_bot_overviews(&self, subject: &Subject) -> Result<Vec<BotOverview>> {
        if !site_admin(subject) {
            return Err(ServiceError::NotFound);
        }
        let mut out = Vec::new();
        for bot in slath_db::bots::list_all(&self.pool).await? {
            out.push(self.overview_of(subject, bot).await?);
        }
        Ok(out)
    }

    async fn overview_of(&self, subject: &Subject, bot: Bot) -> Result<BotOverview> {
        let viewer = match subject {
            Subject::User { id, .. } => Some(*id),
            _ => None,
        };
        let admin = site_admin(subject);
        let (open_mr_count, open_mrs) =
            slath_db::bots::open_mrs(&self.pool, bot.id, viewer, admin, OVERVIEW_ITEMS).await?;
        let pushes =
            slath_db::bots::recent_pushes(&self.pool, bot.id, viewer, admin, OVERVIEW_ITEMS)
                .await?;
        let next_expiry = slath_db::tokens::list_for_user(&self.pool, bot.id)
            .await?
            .into_iter()
            .filter(|t| t.revoked_at.is_none())
            .filter_map(|t| t.expires_at.map(|e| (t.name, e)))
            .filter(|(_, e)| *e > time::OffsetDateTime::now_utc())
            .min_by_key(|(_, e)| *e);
        Ok(BotOverview {
            frozen: slath_db::bots::frozen(&self.pool, bot.id).await?,
            bot,
            open_mr_count,
            open_mrs,
            pushes,
            next_expiry,
        })
    }
}
