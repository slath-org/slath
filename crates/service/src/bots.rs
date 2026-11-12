// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Bot accounts: created and managed by their owner, a person or an
//! organization's owners. Bots never sign in; they act only through the
//! tokens and SSH keys their owner gives them.

use slath_core::Secret;
use slath_core::authz::{Scope, Subject, UserKind, UserState};
use slath_core::credential::CredentialKind;
use slath_core::ids::{RepoId, SshKeyId, TokenId, UserId};
use slath_core::names::NamespaceName;
use slath_db::bots::{Bot, BotOwner};
use slath_db::orgs::NamespaceKind;

use crate::Services;
use crate::accounts::name_error;
use crate::audit::Target;
use crate::error::{FieldError, Result, ServiceError};
use crate::keys::{KeyInfo, check_sudo};
use crate::sessions::Session;
use crate::tokens::{TokenInfo, user_with_scope};
use time::OffsetDateTime;

/// The names of all bots, for marking them wherever names appear. Reloaded
/// when older than [`BotNames::MAX_AGE`] and after a bot is created here.
#[derive(Debug, Default)]
pub struct BotNames {
    inner: std::sync::RwLock<(
        Option<std::time::Instant>,
        std::sync::Arc<std::collections::HashSet<String>>,
    )>,
}

impl BotNames {
    pub const MAX_AGE: std::time::Duration = std::time::Duration::from_secs(10);

    fn fresh(&self) -> Option<std::sync::Arc<std::collections::HashSet<String>>> {
        let g = self.inner.read().ok()?;
        g.0.filter(|t| t.elapsed() < Self::MAX_AGE)
            .map(|_| g.1.clone())
    }

    fn current(&self) -> std::sync::Arc<std::collections::HashSet<String>> {
        self.inner.read().map(|g| g.1.clone()).unwrap_or_default()
    }

    fn store(
        &self,
        names: std::collections::HashSet<String>,
    ) -> std::sync::Arc<std::collections::HashSet<String>> {
        let names = std::sync::Arc::new(names);
        if let Ok(mut g) = self.inner.write() {
            *g = (Some(std::time::Instant::now()), names.clone());
        }
        names
    }

    fn invalidate(&self) {
        if let Ok(mut g) = self.inner.write() {
            g.0 = None;
        }
    }
}

/// Whose bots: the acting person's own, or an organization's.
#[derive(Debug, Clone, Copy)]
pub enum Owner<'a> {
    Me,
    Org(&'a str),
}

/// The person behind `subject`, if it's an active human allowed to act on
/// their account with `scope`. Bots never manage bots.
pub(crate) fn human_with_scope(subject: &Subject, scope: Scope) -> Result<UserId> {
    let user = user_with_scope(subject, scope)?;
    match subject {
        Subject::User {
            kind: UserKind::Human,
            ..
        } => Ok(user),
        _ => Err(ServiceError::Forbidden),
    }
}

fn clean_display_name(v: &str) -> Result<Option<String>> {
    let v = v.trim();
    if v.chars().count() > 100
        || v.chars().any(|c| {
            c.is_control() || matches!(c, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
        })
    {
        return Err(ServiceError::invalid(
            "display_name",
            "invalid",
            "Use at most 100 characters, without control or text-direction characters.",
        ));
    }
    Ok((!v.is_empty()).then(|| v.to_owned()))
}

impl Services {
    /// The lowercased names of all bots (cached; at most
    /// [`BotNames::MAX_AGE`] old). On a database error, the last known set.
    pub async fn bot_names(&self) -> std::sync::Arc<std::collections::HashSet<String>> {
        if let Some(n) = self.bot_names.fresh() {
            return n;
        }
        match slath_db::bots::all_names(&self.pool).await {
            Ok(v) => self.bot_names.store(v.into_iter().collect()),
            Err(e) => {
                tracing::warn!(error = %e, "loading bot names failed");
                self.bot_names.current()
            }
        }
    }

    /// Resolves an owner for managing its bots: the person themselves, or an
    /// organization they own (site administrators may manage any
    /// organization's bots). Anything else is "not found".
    async fn bot_owner(
        &self,
        subject: &Subject,
        owner: Owner<'_>,
        scope: Scope,
    ) -> Result<BotOwner> {
        let user = human_with_scope(subject, scope)?;
        match owner {
            Owner::Me => Ok(BotOwner::User(user)),
            Owner::Org(name) => match slath_db::orgs::resolve_namespace(&self.pool, name).await? {
                Some((_, _, NamespaceKind::Org(org))) => {
                    self.require_org_owner(subject, org)
                        .await
                        .map_err(|_| ServiceError::NotFound)?;
                    Ok(BotOwner::Org(org))
                }
                _ => Err(ServiceError::NotFound),
            },
        }
    }

    /// Creates a bot account for the owner. Its name shares the namespace of
    /// users and organizations.
    pub async fn create_bot(
        &self,
        subject: &Subject,
        owner: Owner<'_>,
        name: &str,
        display_name: &str,
    ) -> Result<Bot> {
        let owner = self.bot_owner(subject, owner, Scope::UserWrite).await?;
        self.require_confirmed(subject).await?;
        self.insert_bot(subject, owner, name, display_name).await
    }

    /// Creates a bot for an owner the caller has checked; audited as the
    /// subject's action.
    pub(crate) async fn insert_bot(
        &self,
        subject: &Subject,
        owner: BotOwner,
        name: &str,
        display_name: &str,
    ) -> Result<Bot> {
        let name = NamespaceName::parse(name.trim()).map_err(|e| {
            let mut f = name_error(&e);
            f.field = "name".to_owned();
            ServiceError::Invalid(vec![f])
        })?;
        let display = clean_display_name(display_name)?;
        let mut tx = self.pool.begin().await.map_err(ServiceError::internal)?;
        let id = slath_db::bots::create(&mut tx, name.as_str(), display.as_deref(), owner)
            .await
            .map_err(|e| match e {
                slath_db::DbError::Conflict(_) => {
                    ServiceError::invalid("name", "taken", "This name is already taken.")
                }
                other => other.into(),
            })?;
        tx.commit().await.map_err(ServiceError::internal)?;
        self.bot_names.invalidate();
        let bot = slath_db::bots::find(&self.pool, name.as_str())
            .await?
            .ok_or_else(|| ServiceError::internal("new bot not found"))?;
        self.audit(
            subject,
            "bot.create",
            Target::new("user", Some(*id.as_uuid())),
            serde_json::json!({"name": bot.name, "owner": bot.owner_name}),
        )
        .await;
        Ok(bot)
    }

    /// The owner's bots (`user:read` for tokens).
    pub async fn list_bots(&self, subject: &Subject, owner: Owner<'_>) -> Result<Vec<Bot>> {
        let owner = self.bot_owner(subject, owner, Scope::UserRead).await?;
        Ok(slath_db::bots::list_for_owner(&self.pool, owner).await?)
    }

    /// Changes a bot's display name (empty removes it). Audited.
    pub async fn set_bot_display_name(
        &self,
        subject: &Subject,
        name: &str,
        display_name: &str,
    ) -> Result<()> {
        let bot = self.managed_bot(subject, name, Scope::UserWrite).await?;
        let display = clean_display_name(display_name)?;
        slath_db::users::update_profile(&self.pool, bot.id, display.as_deref()).await?;
        self.audit(
            subject,
            "bot.update",
            Target::new("user", Some(*bot.id.as_uuid())),
            serde_json::json!({"name": bot.name, "display_name": display}),
        )
        .await;
        Ok(())
    }

    /// A bot the subject manages: its owner, or for an organization's bot,
    /// one of the organization's owners. Everyone else gets "not found".
    pub async fn managed_bot(&self, subject: &Subject, name: &str, scope: Scope) -> Result<Bot> {
        let user = human_with_scope(subject, scope).map_err(|e| match e {
            ServiceError::Forbidden => ServiceError::NotFound,
            e => e,
        })?;
        let bot = slath_db::bots::find(&self.pool, name.trim())
            .await?
            .ok_or(ServiceError::NotFound)?;
        let allowed = match bot.owner {
            BotOwner::User(u) => u == user,
            BotOwner::Org(org) => self.require_org_owner(subject, org).await.is_ok(),
        };
        if allowed && bot.state != UserState::Deleted {
            Ok(bot)
        } else {
            Err(ServiceError::NotFound)
        }
    }
}

/// Scopes a bot token may have: never `user:write` (a bot doesn't manage
/// its own credentials) or `admin`.
pub const BOT_SCOPES: &[Scope] = &[
    Scope::RepoRead,
    Scope::RepoWrite,
    Scope::IssuesWrite,
    Scope::MrsWrite,
    Scope::StatusesWrite,
    Scope::UserRead,
];

/// Most repositories one bot token can name.
pub const BOT_TOKEN_MAX_REPOS: usize = 50;

/// A new bot token.
#[derive(Debug, Clone)]
pub struct NewBotToken {
    pub name: String,
    pub scopes: Vec<String>,
    /// `namespace/name` of each repository the token is restricted to.
    pub repos: Vec<String>,
    /// `None`: the configured default.
    pub expires_in_days: Option<u32>,
}

/// A validated bot token, ready to store.
pub(crate) struct CheckedBotToken {
    name: String,
    scopes: Vec<Scope>,
    repos: Vec<RepoId>,
    expires_at: OffsetDateTime,
}

impl Services {
    /// The default and the longest validity of bot tokens, in days.
    #[must_use]
    pub fn bot_token_days(&self) -> (u32, u32) {
        let max = self.config.auth.bot_token_max_days.max(1);
        (self.config.auth.bot_token_default_days.clamp(1, max), max)
    }

    /// Validity choices offered for bot tokens, in days.
    #[must_use]
    pub fn bot_token_expiry_choices(&self) -> Vec<u32> {
        let (default, max) = self.bot_token_days();
        let mut v: Vec<u32> = [7, 30, 90, default, max]
            .into_iter()
            .filter(|d| *d <= max)
            .collect();
        v.sort_unstable();
        v.dedup();
        v
    }

    /// Checks a new bot token; repositories are resolved as `viewer` sees
    /// them, so a token never names a repository its manager can't read.
    pub(crate) async fn check_bot_token(
        &self,
        viewer: &Subject,
        t: &NewBotToken,
    ) -> Result<CheckedBotToken> {
        let mut errors = Vec::new();
        let name = t.name.trim();
        if name.is_empty() || name.chars().count() > 100 || name.chars().any(char::is_control) {
            errors.push(FieldError::new(
                "name",
                "invalid",
                "Enter a name of 1 to 100 characters.",
            ));
        }
        let mut scopes: Vec<Scope> = Vec::new();
        for raw in &t.scopes {
            match raw.trim().parse::<Scope>() {
                Ok(s) if BOT_SCOPES.contains(&s) => {
                    if !scopes.contains(&s) {
                        scopes.push(s);
                    }
                }
                Ok(s) => errors.push(FieldError::new(
                    "scopes",
                    "not_for_bots",
                    format!("Bot tokens can't have the {s} scope."),
                )),
                Err(_) => errors.push(FieldError::new(
                    "scopes",
                    "unknown",
                    format!("Unknown scope {raw:?}."),
                )),
            }
        }
        if scopes.is_empty() {
            errors.push(FieldError::new(
                "scopes",
                "required",
                "Choose at least one scope.",
            ));
        }
        let mut repos: Vec<RepoId> = Vec::new();
        for raw in t.repos.iter().map(|r| r.trim()).filter(|r| !r.is_empty()) {
            let found = match raw.split_once('/') {
                Some((ns, name)) => match self.resolve_repo(viewer, ns, name).await {
                    Ok(crate::repos::Resolved::Repo(a)) => Some(a.repo.id),
                    _ => None,
                },
                None => None,
            };
            match found {
                Some(id) if !repos.contains(&id) => repos.push(id),
                Some(_) => {}
                None => errors.push(FieldError::new(
                    "repos",
                    "unknown",
                    format!("There's no repository {raw:?} you can see."),
                )),
            }
        }
        if repos.is_empty() {
            errors.push(FieldError::new(
                "repos",
                "required",
                "Name at least one repository: bot tokens work only for the repositories they name.",
            ));
        } else if repos.len() > BOT_TOKEN_MAX_REPOS {
            errors.push(FieldError::new(
                "repos",
                "too_many",
                format!("Name at most {BOT_TOKEN_MAX_REPOS} repositories."),
            ));
        }
        let (default, max) = self.bot_token_days();
        let days = t.expires_in_days.unwrap_or(default);
        if days == 0 || days > max {
            errors.push(FieldError::new(
                "expires",
                "invalid",
                format!("Bot tokens expire after 1 to {max} days."),
            ));
        }
        if !errors.is_empty() {
            return Err(ServiceError::Invalid(errors));
        }
        Ok(CheckedBotToken {
            name: name.to_owned(),
            scopes,
            repos,
            expires_at: OffsetDateTime::now_utc() + time::Duration::days(i64::from(days)),
        })
    }

    pub(crate) async fn store_bot_token(
        &self,
        subject: &Subject,
        bot: &Bot,
        t: CheckedBotToken,
    ) -> Result<(Secret<String>, TokenId)> {
        self.store_token(
            subject,
            bot.id,
            CredentialKind::BotToken,
            &t.name,
            t.scopes,
            Some(&t.repos),
            Some(t.expires_at),
        )
        .await
    }

    /// Creates a token for a bot the session's user manages (sudo mode).
    /// Returns the token, which is shown once.
    pub async fn create_bot_token(
        &self,
        session: &Session,
        bot: &str,
        t: &NewBotToken,
    ) -> Result<(Secret<String>, TokenId)> {
        let bot = self
            .managed_bot(&session.subject, bot, Scope::UserWrite)
            .await?;
        self.require_sudo(session)?;
        let checked = self.check_bot_token(&session.subject, t).await?;
        self.store_bot_token(&session.subject, &bot, checked).await
    }

    /// A bot's tokens, never the secrets.
    pub async fn list_bot_tokens(&self, subject: &Subject, bot: &str) -> Result<Vec<TokenInfo>> {
        let bot = self.managed_bot(subject, bot, Scope::UserRead).await?;
        self.tokens_of(bot.id).await
    }

    /// Revokes one of a bot's tokens.
    pub async fn revoke_bot_token(&self, subject: &Subject, bot: &str, id: TokenId) -> Result<()> {
        let bot = self.managed_bot(subject, bot, Scope::UserWrite).await?;
        self.revoke_token_of(subject, bot.id, id).await
    }

    /// A bot's SSH keys.
    pub async fn list_bot_keys(&self, subject: &Subject, bot: &str) -> Result<Vec<KeyInfo>> {
        let bot = self.managed_bot(subject, bot, Scope::UserRead).await?;
        self.list_keys_of(bot.id).await
    }

    /// Adds an SSH key to a bot (browser sessions need sudo mode).
    pub async fn add_bot_key(
        &self,
        subject: &Subject,
        sudo: bool,
        bot: &str,
        title: &str,
        key: &str,
    ) -> Result<KeyInfo> {
        let bot = self.managed_bot(subject, bot, Scope::UserWrite).await?;
        check_sudo(subject, sudo)?;
        self.add_key_for(subject, bot.id, title, key).await
    }

    /// Removes one of a bot's SSH keys (browser sessions need sudo mode).
    pub async fn remove_bot_key(
        &self,
        subject: &Subject,
        sudo: bool,
        bot: &str,
        id: SshKeyId,
    ) -> Result<()> {
        let bot = self.managed_bot(subject, bot, Scope::UserWrite).await?;
        check_sudo(subject, sudo)?;
        self.remove_key_of(subject, bot.id, id).await
    }

    /// `slath admin create-bot`: creates a bot for the user or organization
    /// `owner` and, if asked, a token (repositories as a site administrator
    /// would see them). Acts as the system.
    pub async fn create_bot_as_system(
        &self,
        owner: &str,
        name: &str,
        display_name: &str,
        token: Option<NewBotToken>,
    ) -> Result<(Bot, Option<Secret<String>>)> {
        let owner = match slath_db::orgs::resolve_namespace(&self.pool, owner.trim()).await? {
            Some((_, _, NamespaceKind::Org(o))) => BotOwner::Org(o),
            Some((_, _, NamespaceKind::User(u))) => {
                let human = slath_db::users::get(&self.pool, u)
                    .await?
                    .is_some_and(|u| u.kind == UserKind::Human && u.state == UserState::Active);
                if !human {
                    return Err(ServiceError::invalid(
                        "owner",
                        "invalid",
                        "A bot's owner must be an active person or an organization.",
                    ));
                }
                BotOwner::User(u)
            }
            None => {
                return Err(ServiceError::invalid(
                    "owner",
                    "unknown",
                    "There's no user or organization with this name.",
                ));
            }
        };
        let checked = match &token {
            Some(t) => Some(self.check_bot_token(&Subject::System, t).await?),
            None => None,
        };
        let bot = self
            .insert_bot(&Subject::System, owner, name, display_name)
            .await?;
        let secret = match checked {
            Some(c) => Some(self.store_bot_token(&Subject::System, &bot, c).await?.0),
            None => None,
        };
        Ok((bot, secret))
    }
}
