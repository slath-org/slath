// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Deploy keys: SSH keys of a repository for machines (a build server, a
//! deployment). A key reads its one repository and, if allowed to push,
//! pushes to it; it's a principal of its own (`Subject::DeployKey`), so
//! it reaches nothing else, not even forks. Repository admins manage them;
//! in a browser, adding and removing needs sudo mode. A fingerprint belongs
//! to one principal, so a key already used by a person or another
//! repository is refused without saying where.

use slath_auth::ssh_key;
use slath_core::authz::{Action, Subject};
use slath_core::ids::SshKeyId;
use slath_db::ssh_keys::DeployKeyRow;

use crate::Services;
use crate::error::{FieldError, Result, ServiceError};
use crate::repos::{RepoAccess, require_sudo_for_sessions};

impl Services {
    /// The repository, if the subject may manage its deploy keys.
    async fn deploy_keys_access(
        &self,
        subject: &Subject,
        ns: &str,
        repo: &str,
    ) -> Result<RepoAccess> {
        self.repo_access(subject, ns, repo, Action::RepoSettings)
            .await
    }

    /// The deploy keys of a repository (admins).
    pub async fn deploy_keys(
        &self,
        subject: &Subject,
        ns: &str,
        repo: &str,
    ) -> Result<(RepoAccess, Vec<DeployKeyRow>)> {
        let a = self.deploy_keys_access(subject, ns, repo).await?;
        let keys = slath_db::ssh_keys::deploy_keys(&self.pool, a.repo.id).await?;
        Ok((a, keys))
    }

    /// Adds a deploy key, read-only unless `can_push`.
    #[allow(clippy::too_many_arguments)]
    pub async fn add_deploy_key(
        &self,
        subject: &Subject,
        sudo: bool,
        (ns, repo): (&str, &str),
        title: &str,
        key: &str,
        can_push: bool,
    ) -> Result<DeployKeyRow> {
        let a = self.deploy_keys_access(subject, ns, repo).await?;
        require_sudo_for_sessions(subject, sudo)?;
        let Subject::User { id: user, .. } = subject else {
            return Err(ServiceError::Unauthenticated);
        };
        let mut errors = Vec::new();
        let parsed = match ssh_key::parse(key) {
            Ok(k) => Some(k),
            Err(e) => {
                errors.push(FieldError::new("key", "invalid", e.to_string()));
                None
            }
        };
        let title = title.trim();
        let title = if title.is_empty() {
            parsed
                .as_ref()
                .and_then(|k| k.comment.clone())
                .unwrap_or_default()
        } else {
            title.to_owned()
        };
        if title.is_empty() || title.chars().count() > 100 || title.chars().any(char::is_control) {
            errors.push(FieldError::new(
                "title",
                "invalid",
                "Enter a title of 1 to 100 characters.",
            ));
        }
        let Some(k) = parsed.filter(|_| errors.is_empty()) else {
            return Err(ServiceError::Invalid(errors));
        };
        let id = SshKeyId::new();
        slath_db::ssh_keys::insert_deploy_key(
            &self.pool,
            &slath_db::ssh_keys::NewDeployKey {
                id,
                repo_id: a.repo.id,
                title: &title,
                key_type: &k.key_type,
                public_key: &k.base64,
                fingerprint: &k.fingerprint,
                can_push,
                added_by: *user,
            },
        )
        .await
        .map_err(|e| match e {
            slath_db::DbError::Conflict(_) => ServiceError::invalid(
                "key",
                "taken",
                "This key is already in use. Generate a new key for this repository.",
            ),
            other => other.into(),
        })?;
        self.audit(
            subject,
            "deploy_key.add",
            crate::audit::Target::repo(*a.repo.id.as_uuid()),
            serde_json::json!({"key": id.to_string(), "title": title, "fingerprint": k.fingerprint, "can_push": can_push}),
        )
        .await;
        slath_db::ssh_keys::deploy_keys(&self.pool, a.repo.id)
            .await?
            .into_iter()
            .find(|d| d.id == id)
            .ok_or(ServiceError::NotFound)
    }

    /// Removes a deploy key; keys of other repositories are "not found".
    pub async fn remove_deploy_key(
        &self,
        subject: &Subject,
        sudo: bool,
        (ns, repo): (&str, &str),
        id: SshKeyId,
    ) -> Result<()> {
        let a = self.deploy_keys_access(subject, ns, repo).await?;
        require_sudo_for_sessions(subject, sudo)?;
        if !slath_db::ssh_keys::delete_deploy_key(&self.pool, a.repo.id, id).await? {
            return Err(ServiceError::NotFound);
        }
        self.audit(
            subject,
            "deploy_key.remove",
            crate::audit::Target::repo(*a.repo.id.as_uuid()),
            serde_json::json!({"key": id.to_string()}),
        )
        .await;
        Ok(())
    }
}
