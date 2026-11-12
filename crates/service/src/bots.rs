// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Bot accounts: created and managed by their owner, a person or an
//! organization's owners. Bots never sign in; they act only through the
//! tokens and SSH keys their owner gives them.

use slath_core::authz::{Scope, Subject, UserKind, UserState};
use slath_core::ids::UserId;
use slath_core::names::NamespaceName;
use slath_db::bots::{Bot, BotOwner};
use slath_db::orgs::NamespaceKind;

use crate::Services;
use crate::accounts::name_error;
use crate::audit::Target;
use crate::error::{Result, ServiceError};
use crate::tokens::user_with_scope;

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
