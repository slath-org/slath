// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Organizations and membership.

use slath_core::authz::{Scope, Subject, UserState};
use slath_core::ids::{OrgId, UserId};
use slath_core::names::NamespaceName;
use slath_db::orgs::{Member, NamespaceKind, Org, OrgRole};

use crate::Services;
use crate::accounts::name_error;
use crate::error::{Result, ServiceError};
use crate::tokens::user_with_scope;

/// The org's profile plus what the viewer may do with it.
#[derive(Debug, Clone)]
pub struct OrgView {
    pub org: Org,
    pub viewer_role: Option<OrgRole>,
    /// Owner or site administrator.
    pub can_manage: bool,
}

fn is_site_admin(subject: &Subject) -> bool {
    matches!(
        subject,
        Subject::User {
            is_site_admin: true,
            state: UserState::Active,
            ..
        }
    )
}

fn clean_text(v: &str, max: usize, field: &str) -> Result<Option<String>> {
    let v = v.trim();
    if v.chars().count() > max
        || v.chars().any(|c| {
            c.is_control() && c != '\n'
                || matches!(c, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
        })
    {
        return Err(ServiceError::invalid(
            field,
            "invalid",
            format!("Use at most {max} characters, without control or text-direction characters."),
        ));
    }
    Ok((!v.is_empty()).then(|| v.to_owned()))
}

impl Services {
    /// Creates an organization; the creator becomes its owner. Any user may
    /// create one unless `auth.users_can_create_orgs` is off (then site
    /// administrators only).
    pub async fn create_org(
        &self,
        subject: &Subject,
        name: &str,
        display_name: &str,
    ) -> Result<OrgId> {
        let user = crate::bots::human_with_scope(subject, Scope::UserWrite)?;
        self.require_confirmed(subject).await?;
        if !self.config.auth.users_can_create_orgs && !is_site_admin(subject) {
            return Err(ServiceError::Forbidden);
        }
        let name = NamespaceName::parse(name.trim()).map_err(|e| {
            ServiceError::Invalid(vec![{
                let mut f = name_error(&e);
                f.field = "name".to_owned();
                f
            }])
        })?;
        let display = clean_text(display_name, 100, "display_name")?;
        let mut tx = self.pool.begin().await.map_err(ServiceError::internal)?;
        let id = slath_db::orgs::create(&mut tx, name.as_str(), display.as_deref(), user)
            .await
            .map_err(|e| match e {
                slath_db::DbError::Conflict(_) => {
                    ServiceError::invalid("name", "taken", "This name is already taken.")
                }
                other => other.into(),
            })?;
        tx.commit().await.map_err(ServiceError::internal)?;
        self.audit(
            subject,
            "org.create",
            crate::audit::Target::org(*id.as_uuid()),
            serde_json::json!({"name": name.as_str()}),
        )
        .await;
        Ok(id)
    }

    /// The acting user's role in an org (sessions, or tokens of any scope).
    pub(crate) async fn org_role(&self, subject: &Subject, org: OrgId) -> Result<Option<OrgRole>> {
        match subject {
            Subject::User {
                id,
                state: UserState::Active,
                ..
            } => Ok(slath_db::orgs::role_of(&self.pool, org, *id).await?),
            _ => Ok(None),
        }
    }

    /// Org profiles are public.
    pub async fn org_view(&self, subject: &Subject, org: OrgId) -> Result<OrgView> {
        let org = slath_db::orgs::get(&self.pool, org)
            .await?
            .ok_or(ServiceError::NotFound)?;
        let viewer_role = self.org_role(subject, org.id).await?;
        Ok(OrgView {
            can_manage: viewer_role == Some(OrgRole::Owner) || is_site_admin(subject),
            viewer_role,
            org,
        })
    }

    /// Member lists are visible to members and site administrators.
    pub async fn org_members(&self, subject: &Subject, org: OrgId) -> Result<Vec<Member>> {
        // Membership is account data: tokens need `user:read`.
        user_with_scope(subject, Scope::UserRead)?;
        if self.org_role(subject, org).await?.is_none() && !is_site_admin(subject) {
            return Err(ServiceError::Forbidden);
        }
        Ok(slath_db::orgs::members(&self.pool, org).await?)
    }

    /// Fails unless the subject owns the org or is a site administrator.
    /// Tokens need `user:write`.
    pub(crate) async fn require_org_owner(&self, subject: &Subject, org: OrgId) -> Result<UserId> {
        let user = user_with_scope(subject, Scope::UserWrite)?;
        if is_site_admin(subject) || self.org_role(subject, org).await? == Some(OrgRole::Owner) {
            Ok(user)
        } else {
            Err(ServiceError::Forbidden)
        }
    }

    pub async fn update_org(
        &self,
        subject: &Subject,
        org: OrgId,
        display_name: &str,
        description: &str,
    ) -> Result<()> {
        self.require_org_owner(subject, org).await?;
        let display = clean_text(display_name, 100, "display_name")?;
        let desc = clean_text(description, 500, "description")?;
        slath_db::orgs::update(&self.pool, org, display.as_deref(), desc.as_deref()).await?;
        Ok(())
    }

    /// Adds a member or changes a member's role (owners only). Demoting the
    /// last owner is refused.
    pub async fn set_org_member(
        &self,
        subject: &Subject,
        org: OrgId,
        username: &str,
        role: OrgRole,
    ) -> Result<()> {
        self.require_org_owner(subject, org).await?;
        let target = match slath_db::orgs::resolve_namespace(&self.pool, username.trim()).await? {
            Some((_, _, NamespaceKind::User(u))) => u,
            _ => {
                return Err(ServiceError::invalid(
                    "username",
                    "unknown",
                    "There's no user with this name.",
                ));
            }
        };
        if role == OrgRole::Owner
            && slath_db::users::get(&self.pool, target)
                .await?
                .is_some_and(|u| u.kind == slath_core::authz::UserKind::Bot)
        {
            return Err(ServiceError::invalid(
                "username",
                "bot",
                "Bots can't own organizations.",
            ));
        }
        let mut tx = self.pool.begin().await.map_err(ServiceError::internal)?;
        slath_db::orgs::lock(&mut tx, org).await?;
        let current = slath_db::orgs::role_of(&mut *tx, org, target).await?;
        if current == Some(OrgRole::Owner)
            && role != OrgRole::Owner
            && slath_db::orgs::owner_count(&mut *tx, org).await? <= 1
        {
            return Err(ServiceError::Conflict(
                "An organization needs at least one owner.".to_owned(),
            ));
        }
        slath_db::orgs::upsert_member(&mut *tx, org, target, role).await?;
        tx.commit().await.map_err(ServiceError::internal)?;
        self.audit(
            subject,
            "org.member.set",
            crate::audit::Target::org(*org.as_uuid()),
            serde_json::json!({"username": username.trim(), "role": role.as_str()}),
        )
        .await;
        self.review_org_forks(org).await;
        Ok(())
    }

    /// Removes a member (owners), or leaves (any member). The last owner
    /// can't leave.
    pub async fn remove_org_member(
        &self,
        subject: &Subject,
        org: OrgId,
        username: &str,
    ) -> Result<()> {
        let target = match slath_db::orgs::resolve_namespace(&self.pool, username.trim()).await? {
            Some((_, _, NamespaceKind::User(u))) => u,
            _ => return Err(ServiceError::NotFound),
        };
        let leaving = matches!(subject, Subject::User { id, .. } if *id == target);
        if leaving {
            user_with_scope(subject, Scope::UserWrite)?;
        } else {
            self.require_org_owner(subject, org).await?;
        }
        let mut tx = self.pool.begin().await.map_err(ServiceError::internal)?;
        slath_db::orgs::lock(&mut tx, org).await?;
        let current = slath_db::orgs::role_of(&mut *tx, org, target)
            .await?
            .ok_or(ServiceError::NotFound)?;
        if current == OrgRole::Owner && slath_db::orgs::owner_count(&mut *tx, org).await? <= 1 {
            return Err(ServiceError::Conflict(
                "An organization needs at least one owner.".to_owned(),
            ));
        }
        slath_db::orgs::remove_member(&mut *tx, org, target).await?;
        slath_db::teams::remove_from_org_teams(&mut *tx, org, target).await?;
        tx.commit().await.map_err(ServiceError::internal)?;
        self.audit(
            subject,
            "org.member.remove",
            crate::audit::Target::org(*org.as_uuid()),
            serde_json::json!({"username": username.trim()}),
        )
        .await;
        self.review_org_forks(org).await;
        Ok(())
    }

    /// Resolves a namespace name.
    pub async fn resolve_namespace(&self, name: &str) -> Result<Option<(String, NamespaceKind)>> {
        if NamespaceName::parse(name).is_err() {
            return Ok(None);
        }
        Ok(slath_db::orgs::resolve_namespace(&self.pool, name)
            .await?
            .map(|(_, n, k)| (n, k)))
    }
}
