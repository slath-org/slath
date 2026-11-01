// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Teams and organization member policies.
//!
//! Organization owners create teams, add organization members to them, and
//! set the member policies (the role every member gets, and whether members
//! need a second factor). Repository administrators give a team of the
//! owning organization a role on the repository. Members see the teams.
//! Every change takes effect on the next request: roles are computed per
//! request from the grants.

use slath_core::authz::{Action, Role, Scope, Subject};
use slath_core::ids::OrgId;
use slath_core::names::NamespaceName;
use slath_db::orgs::{NamespaceKind, OrgRole};

use crate::Services;
use crate::error::{Result, ServiceError};
use crate::tokens::user_with_scope;

/// A team with its members and repositories (for its page).
#[derive(Debug, Clone)]
pub struct TeamDetail {
    pub team: slath_db::teams::Team,
    pub members: Vec<String>,
    pub repos: Vec<(String, Role)>,
}

fn is_site_admin(subject: &Subject) -> bool {
    matches!(
        subject,
        Subject::User {
            is_site_admin: true,
            ..
        }
    )
}

impl Services {
    async fn require_member_or_admin(&self, subject: &Subject, org: OrgId) -> Result<()> {
        user_with_scope(subject, Scope::UserRead)?;
        if is_site_admin(subject) || self.org_role(subject, org).await?.is_some() {
            Ok(())
        } else {
            Err(ServiceError::NotFound)
        }
    }

    async fn require_owner(&self, subject: &Subject, org: OrgId) -> Result<()> {
        user_with_scope(subject, Scope::UserWrite)?;
        if is_site_admin(subject) || self.org_role(subject, org).await? == Some(OrgRole::Owner) {
            Ok(())
        } else if self.org_role(subject, org).await?.is_some() {
            Err(ServiceError::Forbidden)
        } else {
            Err(ServiceError::NotFound)
        }
    }

    /// The organization's teams (members and site administrators).
    pub async fn teams(&self, subject: &Subject, org: OrgId) -> Result<Vec<slath_db::teams::Team>> {
        self.require_member_or_admin(subject, org).await?;
        Ok(slath_db::teams::list(&self.pool, org).await?)
    }

    /// One team with members and repositories (members and site
    /// administrators; repositories the viewer can't read are left out).
    pub async fn team(&self, subject: &Subject, org: OrgId, name: &str) -> Result<TeamDetail> {
        self.require_member_or_admin(subject, org).await?;
        let team = slath_db::teams::find(&self.pool, org, name)
            .await?
            .ok_or(ServiceError::NotFound)?;
        let members = slath_db::teams::members(&self.pool, team.id)
            .await?
            .into_iter()
            .map(|(_, n)| n)
            .collect();
        let mut repos = Vec::new();
        for (id, full, role) in slath_db::teams::repos(&self.pool, team.id).await? {
            if self
                .repo_access_by_id(subject, id, Action::RepoRead)
                .await
                .is_ok()
            {
                repos.push((full, role));
            }
        }
        Ok(TeamDetail {
            team,
            members,
            repos,
        })
    }

    /// Creates a team (owners).
    pub async fn create_team(
        &self,
        subject: &Subject,
        org: OrgId,
        name: &str,
        description: &str,
    ) -> Result<()> {
        self.require_owner(subject, org).await?;
        let name = name.trim();
        if NamespaceName::parse(name).is_err() {
            return Err(ServiceError::invalid(
                "name",
                "invalid",
                "Use letters, digits, dots, dashes, and underscores (up to 39 characters).",
            ));
        }
        let description = description.trim();
        if description.chars().count() > 300 || description.chars().any(char::is_control) {
            return Err(ServiceError::invalid(
                "description",
                "invalid",
                "Use at most 300 characters.",
            ));
        }
        slath_db::teams::create(
            &self.pool,
            org,
            name,
            (!description.is_empty()).then_some(description),
        )
        .await
        .map_err(|e| match e {
            slath_db::DbError::Conflict(_) => {
                ServiceError::invalid("name", "taken", "A team with this name exists.")
            }
            other => other.into(),
        })?;
        self.audit(
            subject,
            "team.create",
            crate::audit::Target::org(*org.as_uuid()),
            serde_json::json!({"team": name}),
        )
        .await;
        Ok(())
    }

    /// Deletes a team; its members lose the access it gave at once.
    pub async fn delete_team(&self, subject: &Subject, org: OrgId, name: &str) -> Result<()> {
        self.require_owner(subject, org).await?;
        let team = slath_db::teams::find(&self.pool, org, name)
            .await?
            .ok_or(ServiceError::NotFound)?;
        slath_db::teams::delete(&self.pool, team.id).await?;
        self.audit(
            subject,
            "team.delete",
            crate::audit::Target::org(*org.as_uuid()),
            serde_json::json!({"team": team.name}),
        )
        .await;
        Ok(())
    }

    /// Adds an organization member to a team (owners).
    pub async fn add_team_member(
        &self,
        subject: &Subject,
        org: OrgId,
        team: &str,
        username: &str,
    ) -> Result<()> {
        self.require_owner(subject, org).await?;
        let team = slath_db::teams::find(&self.pool, org, team)
            .await?
            .ok_or(ServiceError::NotFound)?;
        let user = match slath_db::orgs::resolve_namespace(&self.pool, username.trim()).await? {
            Some((_, _, NamespaceKind::User(u))) => u,
            _ => {
                return Err(ServiceError::invalid(
                    "username",
                    "unknown",
                    "There's no user with this name.",
                ));
            }
        };
        if slath_db::orgs::role_of(&self.pool, org, user)
            .await?
            .is_none()
        {
            return Err(ServiceError::invalid(
                "username",
                "not_member",
                "Add the user to the organization first.",
            ));
        }
        slath_db::teams::add_member(&self.pool, team.id, user).await?;
        self.audit(
            subject,
            "team.member.add",
            crate::audit::Target::org(*org.as_uuid()),
            serde_json::json!({"team": team.name, "username": username.trim()}),
        )
        .await;
        Ok(())
    }

    pub async fn remove_team_member(
        &self,
        subject: &Subject,
        org: OrgId,
        team: &str,
        username: &str,
    ) -> Result<()> {
        self.require_owner(subject, org).await?;
        let team = slath_db::teams::find(&self.pool, org, team)
            .await?
            .ok_or(ServiceError::NotFound)?;
        let Some((_, _, NamespaceKind::User(user))) =
            slath_db::orgs::resolve_namespace(&self.pool, username.trim()).await?
        else {
            return Err(ServiceError::NotFound);
        };
        if !slath_db::teams::remove_member(&self.pool, team.id, user).await? {
            return Err(ServiceError::NotFound);
        }
        self.audit(
            subject,
            "team.member.remove",
            crate::audit::Target::org(*org.as_uuid()),
            serde_json::json!({"team": team.name, "username": username.trim()}),
        )
        .await;
        Ok(())
    }

    /// Sets the organization's member policies (owners).
    pub async fn set_org_policies(
        &self,
        subject: &Subject,
        org: OrgId,
        default_member_role: &str,
        require_mfa: bool,
    ) -> Result<()> {
        self.require_owner(subject, org).await?;
        if !matches!(default_member_role, "none" | "read") {
            return Err(ServiceError::invalid(
                "default_member_role",
                "invalid",
                "Choose no access or read access.",
            ));
        }
        slath_db::orgs::set_policies(&self.pool, org, default_member_role, require_mfa).await?;
        self.audit(
            subject,
            "org.policies",
            crate::audit::Target::org(*org.as_uuid()),
            serde_json::json!({"default_member_role": default_member_role, "require_mfa": require_mfa}),
        )
        .await;
        Ok(())
    }

    /// Teams with access to a repository (repository admins).
    pub async fn repo_teams(
        &self,
        subject: &Subject,
        ns: &str,
        repo: &str,
    ) -> Result<Vec<(String, Role)>> {
        let access = self
            .repo_access(subject, ns, repo, Action::CollaboratorsManage)
            .await?;
        Ok(slath_db::teams::repo_teams(&self.pool, access.repo.id).await?)
    }

    /// Gives a team of the owning organization a role on a repository, or
    /// removes it (`role: None`). Repository admins.
    pub async fn set_repo_team(
        &self,
        subject: &Subject,
        ns: &str,
        repo: &str,
        team: &str,
        role: Option<Role>,
    ) -> Result<()> {
        let access = self
            .repo_access(subject, ns, repo, Action::CollaboratorsManage)
            .await?;
        let not_found = || {
            ServiceError::invalid(
                "team",
                "unknown",
                "There's no team with this name in the organization.",
            )
        };
        let Some((_, _, NamespaceKind::Org(org))) =
            slath_db::orgs::resolve_namespace(&self.pool, &access.repo.namespace).await?
        else {
            return Err(ServiceError::invalid(
                "team",
                "no_org",
                "Only repositories of organizations can give teams access.",
            ));
        };
        let t = slath_db::teams::find(&self.pool, org, team.trim())
            .await?
            .ok_or_else(not_found)?;
        match role {
            Some(r) => {
                slath_db::teams::set_repo_access(&self.pool, access.repo.id, t.id, r).await?
            }
            None => {
                if !slath_db::teams::remove_repo_access(&self.pool, access.repo.id, t.id).await? {
                    return Err(ServiceError::NotFound);
                }
            }
        }
        self.audit(
            subject,
            "repo.team.set",
            crate::audit::Target::repo(*access.repo.id.as_uuid()),
            serde_json::json!({"team": t.name, "role": role.map(slath_db::repos::role_str)}),
        )
        .await;
        Ok(())
    }
}
