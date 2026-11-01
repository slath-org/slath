// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Site administration. Everything here is for active site administrators
//! with a browser session; everyone else gets "not found". Changes need
//! sudo mode and are audited.

use slath_core::authz::{Credential, Subject, UserState};
use slath_core::ids::UserId;
use slath_db::orgs::NamespaceKind;

use crate::Services;
use crate::audit::Target;
use crate::error::{Result, ServiceError};

/// Instance health for the admin page.
#[derive(Debug, Clone)]
pub struct Health {
    pub git_version: Result<String, String>,
    pub schema: i32,
    pub required_schema: i32,
    pub jobs: Vec<(String, String, i64)>,
}

impl Services {
    /// Fails with "not found" unless the subject is an active site
    /// administrator signed in with a browser session.
    pub fn require_site_admin(&self, subject: &Subject) -> Result<()> {
        match subject {
            Subject::User {
                is_site_admin: true,
                state: UserState::Active,
                credential: Credential::Session,
                ..
            } => Ok(()),
            _ => Err(ServiceError::NotFound),
        }
    }

    async fn admin_target(&self, name: &str) -> Result<UserId> {
        match slath_db::orgs::resolve_namespace(&self.pool, name).await? {
            Some((_, _, NamespaceKind::User(u))) => Ok(u),
            _ => Err(ServiceError::NotFound),
        }
    }

    fn not_self(subject: &Subject, target: UserId) -> Result<()> {
        if matches!(subject, Subject::User { id, .. } if *id == target) {
            return Err(ServiceError::Conflict(
                "You can't change this for your own account.".to_owned(),
            ));
        }
        Ok(())
    }

    pub async fn admin_users(
        &self,
        subject: &Subject,
        offset: usize,
        limit: usize,
    ) -> Result<Vec<slath_db::users::User>> {
        self.require_site_admin(subject)?;
        Ok(slath_db::users::list_all(&self.pool, offset as i64, limit as i64).await?)
    }

    /// Suspends or reactivates a user. Suspension signs out all sessions and
    /// revokes all tokens at once.
    pub async fn admin_set_suspended(
        &self,
        subject: &Subject,
        sudo: bool,
        name: &str,
        suspended: bool,
    ) -> Result<()> {
        self.require_site_admin(subject)?;
        if !sudo {
            return Err(ServiceError::SudoRequired);
        }
        let user = self.admin_target(name).await?;
        Self::not_self(subject, user)?;
        let mut tx = self.pool.begin().await.map_err(ServiceError::internal)?;
        slath_db::users::set_state(&mut *tx, user, !suspended).await?;
        if suspended {
            slath_db::sessions::delete_all_for_user(&mut *tx, user, None).await?;
            slath_db::tokens::revoke_all_for_user(&mut *tx, user).await?;
        }
        tx.commit().await.map_err(ServiceError::internal)?;
        let action = if suspended {
            "admin.user.suspend"
        } else {
            "admin.user.unsuspend"
        };
        self.audit(
            subject,
            action,
            Target::new("user", Some(*user.as_uuid())),
            serde_json::json!({"username": name}),
        )
        .await;
        Ok(())
    }

    /// Grants or revokes site administration.
    pub async fn admin_set_admin(
        &self,
        subject: &Subject,
        sudo: bool,
        name: &str,
        admin: bool,
    ) -> Result<()> {
        self.require_site_admin(subject)?;
        if !sudo {
            return Err(ServiceError::SudoRequired);
        }
        let user = self.admin_target(name).await?;
        Self::not_self(subject, user)?;
        slath_db::users::set_admin(&self.pool, user, admin).await?;
        let action = if admin {
            "admin.user.grant_admin"
        } else {
            "admin.user.revoke_admin"
        };
        self.audit(
            subject,
            action,
            Target::new("user", Some(*user.as_uuid())),
            serde_json::json!({"username": name}),
        )
        .await;
        Ok(())
    }

    pub async fn admin_orgs(
        &self,
        subject: &Subject,
        offset: usize,
        limit: usize,
    ) -> Result<Vec<slath_db::orgs::Org>> {
        self.require_site_admin(subject)?;
        Ok(slath_db::orgs::list_all(&self.pool, offset as i64, limit as i64).await?)
    }

    pub async fn admin_repos(
        &self,
        subject: &Subject,
        offset: usize,
        limit: usize,
    ) -> Result<Vec<slath_db::repos::Repo>> {
        self.require_site_admin(subject)?;
        Ok(slath_db::repos::list_all(&self.pool, offset as i64, limit as i64).await?)
    }

    /// Audit events for site administrators (any) or organization owners (their org).
    pub async fn audit_events(
        &self,
        subject: &Subject,
        filter: &slath_db::audit::Filter,
        offset: usize,
        limit: usize,
    ) -> Result<Vec<slath_db::audit::Event>> {
        if self.require_site_admin(subject).is_err() {
            let Some(org) = filter.org_id else {
                return Err(ServiceError::NotFound);
            };
            let Subject::User {
                id,
                state: UserState::Active,
                ..
            } = subject
            else {
                return Err(ServiceError::NotFound);
            };
            let role =
                slath_db::orgs::role_of(&self.pool, slath_core::ids::OrgId::from_uuid(org), *id)
                    .await?;
            if role != Some(slath_db::orgs::OrgRole::Owner) {
                return Err(ServiceError::NotFound);
            }
        }
        Ok(slath_db::audit::list(&self.pool, filter, offset as i64, limit as i64).await?)
    }

    pub async fn health(&self, subject: &Subject) -> Result<Health> {
        self.require_site_admin(subject)?;
        Ok(Health {
            git_version: self
                .runner
                .check_version()
                .await
                .map(|v| v.to_string())
                .map_err(|e| e.to_string()),
            schema: slath_db::schema_version(&self.pool).await?.unwrap_or(0),
            required_schema: slath_db::REQUIRED_SCHEMA,
            jobs: slath_db::jobs::stats(&self.pool).await?,
        })
    }
}
