// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! SSH keys of the signed-in user.

use slath_auth::ssh_key;
use slath_core::authz::{Credential, Scope, Subject};
use slath_core::ids::{SshKeyId, UserId};
use time::OffsetDateTime;

use crate::Services;
use crate::error::{FieldError, Result, ServiceError};
use crate::tokens::user_with_scope;

#[derive(Debug, Clone)]
pub struct KeyInfo {
    pub id: SshKeyId,
    pub title: String,
    pub key_type: String,
    pub fingerprint: String,
    pub last_used_at: Option<OffsetDateTime>,
    pub created_at: OffsetDateTime,
}

/// Adding or removing keys from a browser needs sudo mode; tokens need the
/// `user:write` scope instead.
pub(crate) fn check_sudo(subject: &Subject, sudo: bool) -> Result<()> {
    match subject {
        Subject::User {
            credential: Credential::Session,
            ..
        } if !sudo => Err(ServiceError::SudoRequired),
        _ => Ok(()),
    }
}

impl Services {
    pub async fn list_keys(&self, subject: &Subject) -> Result<Vec<KeyInfo>> {
        let user = user_with_scope(subject, Scope::UserRead)?;
        self.list_keys_of(user).await
    }

    /// The keys of `user`'s account (the subject is authorized already).
    pub(crate) async fn list_keys_of(&self, user: UserId) -> Result<Vec<KeyInfo>> {
        Ok(slath_db::ssh_keys::list_for_user(&self.pool, user)
            .await?
            .into_iter()
            .map(|k| KeyInfo {
                id: k.id,
                title: k.title,
                key_type: k.key_type,
                fingerprint: k.fingerprint,
                last_used_at: k.last_used_at,
                created_at: k.created_at,
            })
            .collect())
    }

    /// Adds a key. `sudo` says whether a browser session is in sudo mode.
    /// A key registered to any account is rejected without saying whose.
    pub async fn add_key(
        &self,
        subject: &Subject,
        sudo: bool,
        title: &str,
        key: &str,
    ) -> Result<KeyInfo> {
        let user = user_with_scope(subject, Scope::UserWrite)?;
        check_sudo(subject, sudo)?;
        self.add_key_for(subject, user, title, key).await
    }

    /// Adds a key to `user`'s account (the subject is authorized already).
    pub(crate) async fn add_key_for(
        &self,
        subject: &Subject,
        user: UserId,
        title: &str,
        key: &str,
    ) -> Result<KeyInfo> {
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
        slath_db::ssh_keys::insert(
            &self.pool,
            &slath_db::ssh_keys::NewKey {
                id,
                user_id: user,
                title: &title,
                key_type: &k.key_type,
                public_key: &k.base64,
                fingerprint: &k.fingerprint,
            },
        )
        .await
        .map_err(|e| match e {
            slath_db::DbError::Conflict(_) => ServiceError::invalid(
                "key",
                "taken",
                "This key is already in use. Generate a new key for this account.",
            ),
            other => other.into(),
        })?;
        self.audit(
            subject,
            "ssh_key.add",
            crate::audit::Target::new("ssh_key", Some(*id.as_uuid())),
            serde_json::json!({"title": title, "fingerprint": k.fingerprint}),
        )
        .await;
        Ok(KeyInfo {
            id,
            title,
            key_type: k.key_type,
            fingerprint: k.fingerprint,
            last_used_at: None,
            created_at: OffsetDateTime::now_utc(),
        })
    }

    /// Removes one of the user's keys; other users' keys are "not found".
    pub async fn remove_key(&self, subject: &Subject, sudo: bool, id: SshKeyId) -> Result<()> {
        let user = user_with_scope(subject, Scope::UserWrite)?;
        check_sudo(subject, sudo)?;
        self.remove_key_of(subject, user, id).await
    }

    /// Removes a key of `user`'s account (the subject is authorized already).
    pub(crate) async fn remove_key_of(
        &self,
        subject: &Subject,
        user: UserId,
        id: SshKeyId,
    ) -> Result<()> {
        if slath_db::ssh_keys::delete_for_user(&self.pool, user, id).await? {
            self.audit(
                subject,
                "ssh_key.remove",
                crate::audit::Target::new("ssh_key", Some(*id.as_uuid())),
                serde_json::json!({}),
            )
            .await;
            Ok(())
        } else {
            Err(ServiceError::NotFound)
        }
    }
}
