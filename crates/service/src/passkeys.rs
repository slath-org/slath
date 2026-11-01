// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Passkeys (WebAuthn): registration and removal, and their use as a
//! second factor, for passwordless sign-in, and to confirm sudo mode.
//!
//! The relying party ID is the host of `public_url` and the only accepted
//! origin is `public_url` itself. User verification is required for every
//! passkey, so a passkey is a complete sign-in on its own. Ceremony state
//! (the challenge) is kept server-side, bound to the user where there is
//! one, and can be finished once, within five minutes.

use slath_auth::token;
use slath_core::Secret;
use slath_core::authz::{Subject, UserKind, UserState};
use slath_core::ids::UserId;
use slath_db::sessions::AuthLevel;
use time::OffsetDateTime;
use webauthn_rs::prelude::{
    DiscoverableAuthentication, Passkey, PasskeyAuthentication, PasskeyRegistration,
    PublicKeyCredential, RegisterPublicKeyCredential, Url, Webauthn, WebauthnBuilder,
};

use crate::Services;
use crate::error::{Result, ServiceError};
use crate::sessions::{NewSessionInfo, Session, SessionCookie};

const CEREMONY_TTL: time::Duration = time::Duration::minutes(5);
/// Passkeys per account.
const MAX_PASSKEYS: usize = 20;

/// The relying party for `public_url`, or `None` if passkeys can't work
/// there (an IP address instead of a host name).
#[must_use]
pub fn relying_party(public_url: &slath_core::config::PublicUrl) -> Option<Webauthn> {
    let origin = Url::parse(public_url.origin()).ok()?;
    let host = origin.domain()?.to_owned();
    WebauthnBuilder::new(&host, &origin)
        .ok()?
        .rp_name("Slath")
        .build()
        .ok()
}

/// A passkey as the settings page lists it.
#[derive(Debug, Clone)]
pub struct PasskeyInfo {
    pub id: uuid::Uuid,
    pub name: String,
    pub created_at: OffsetDateTime,
    pub last_used_at: Option<OffsetDateTime>,
}

/// Options for `navigator.credentials` plus the ceremony token to send back.
pub struct Ceremony {
    pub options: serde_json::Value,
    pub token: Secret<String>,
}

fn invalid(e: impl std::fmt::Display) -> ServiceError {
    tracing::info!(error = %e, "passkey ceremony failed");
    ServiceError::invalid(
        "passkey",
        "invalid",
        "The passkey couldn't be verified. Try again.",
    )
}

impl Services {
    fn webauthn(&self) -> Result<&Webauthn> {
        self.webauthn.as_deref().ok_or(ServiceError::NotFound)
    }

    /// True if passkeys work on this instance.
    #[must_use]
    pub fn passkeys_available(&self) -> bool {
        self.webauthn.is_some()
    }

    async fn new_ceremony(
        &self,
        user: Option<UserId>,
        kind: &str,
        state: serde_json::Value,
    ) -> Result<Secret<String>> {
        let t = token::new_session_id().map_err(ServiceError::internal)?;
        slath_db::mfa::insert_ceremony(
            &self.pool,
            &token::storage_hash(t.expose()),
            user,
            kind,
            &state,
            OffsetDateTime::now_utc() + CEREMONY_TTL,
        )
        .await?;
        Ok(t)
    }

    async fn take_ceremony(
        &self,
        t: &str,
        kind: &str,
    ) -> Result<(Option<UserId>, serde_json::Value)> {
        if !token::is_session_id(t) {
            return Err(invalid("malformed ceremony token"));
        }
        slath_db::mfa::take_ceremony(&self.pool, &token::storage_hash(t), kind)
            .await?
            .ok_or_else(|| invalid("unknown or expired ceremony"))
    }

    async fn user_passkeys(&self, user: UserId) -> Result<Vec<(uuid::Uuid, Passkey)>> {
        slath_db::mfa::passkeys(&self.pool, user)
            .await?
            .into_iter()
            .map(|p| {
                serde_json::from_value::<Passkey>(p.passkey)
                    .map(|k| (p.id, k))
                    .map_err(ServiceError::internal)
            })
            .collect()
    }

    /// The user's passkeys.
    pub async fn passkey_list(&self, user: UserId) -> Result<Vec<PasskeyInfo>> {
        Ok(slath_db::mfa::passkeys(&self.pool, user)
            .await?
            .into_iter()
            .map(|p| PasskeyInfo {
                id: p.id,
                name: p.name,
                created_at: p.created_at,
                last_used_at: p.last_used_at,
            })
            .collect())
    }

    /// Starts adding a passkey (sudo mode).
    pub async fn passkey_register_start(&self, session: &Session) -> Result<Ceremony> {
        self.require_sudo(session)?;
        let wa = self.webauthn()?;
        let user = session.user_id();
        let existing = self.user_passkeys(user).await?;
        if existing.len() >= MAX_PASSKEYS {
            return Err(ServiceError::Conflict(format!(
                "An account can have at most {MAX_PASSKEYS} passkeys."
            )));
        }
        let exclude = existing.iter().map(|(_, k)| k.cred_id().clone()).collect();
        let (ccr, state) = wa
            .start_passkey_registration(
                *user.as_uuid(),
                &session.row.user_name,
                &session.row.user_name,
                Some(exclude),
            )
            .map_err(ServiceError::internal)?;
        let mut options = serde_json::to_value(&ccr).map_err(ServiceError::internal)?;
        // Ask for a discoverable credential, so the passkey also works
        // without a username (passwordless sign-in).
        if let Some(sel) = options
            .pointer_mut("/publicKey/authenticatorSelection")
            .and_then(serde_json::Value::as_object_mut)
        {
            sel.insert("residentKey".to_owned(), "preferred".into());
            sel.insert("requireResidentKey".to_owned(), false.into());
        }
        let token = self
            .new_ceremony(
                Some(user),
                "register",
                serde_json::to_value(&state).map_err(ServiceError::internal)?,
            )
            .await?;
        Ok(Ceremony { options, token })
    }

    /// Finishes adding a passkey with the browser's answer.
    pub async fn passkey_register_finish(
        &self,
        session: &Session,
        ceremony: &str,
        name: &str,
        credential: &str,
    ) -> Result<()> {
        self.require_sudo(session)?;
        let wa = self.webauthn()?;
        let name = name.trim();
        if name.is_empty() || name.chars().count() > 100 || name.chars().any(char::is_control) {
            return Err(ServiceError::invalid(
                "name",
                "invalid",
                "Name the passkey (up to 100 characters), e.g. \"Laptop\".",
            ));
        }
        let user = session.user_id();
        let (owner, state) = self.take_ceremony(ceremony, "register").await?;
        if owner != Some(user) {
            return Err(invalid("ceremony of another user"));
        }
        let state: PasskeyRegistration = serde_json::from_value(state).map_err(invalid)?;
        let cred: RegisterPublicKeyCredential =
            serde_json::from_str(credential).map_err(invalid)?;
        let key = wa
            .finish_passkey_registration(&cred, &state)
            .map_err(invalid)?;
        slath_db::mfa::insert_passkey(
            &self.pool,
            user,
            {
                let id: &[u8] = key.cred_id().as_ref();
                id
            },
            &serde_json::to_value(&key).map_err(ServiceError::internal)?,
            name,
        )
        .await
        .map_err(|e| match e {
            slath_db::DbError::Conflict(_) => {
                ServiceError::invalid("passkey", "taken", "This passkey is already registered.")
            }
            other => other.into(),
        })?;
        self.audit(
            &session.subject,
            "mfa.passkey_add",
            crate::audit::Target::new("user", Some(*user.as_uuid())),
            serde_json::json!({"name": name}),
        )
        .await;
        self.notify_security_change(
            user,
            &format!("A passkey ({name}) was added to your account."),
        )
        .await;
        Ok(())
    }

    /// Removes a passkey (sudo mode).
    pub async fn passkey_delete(&self, session: &Session, id: uuid::Uuid) -> Result<()> {
        self.require_sudo(session)?;
        let user = session.user_id();
        if !slath_db::mfa::delete_passkey(&self.pool, user, id).await? {
            return Err(ServiceError::NotFound);
        }
        self.audit(
            &session.subject,
            "mfa.passkey_remove",
            crate::audit::Target::new("user", Some(*user.as_uuid())),
            serde_json::json!({}),
        )
        .await;
        self.notify_security_change(user, "A passkey was removed from your account.")
            .await;
        Ok(())
    }

    /// Starts using one of `user`'s passkeys (second factor, sudo).
    pub async fn passkey_auth_start(&self, user: UserId) -> Result<Ceremony> {
        let wa = self.webauthn()?;
        let keys: Vec<Passkey> = self
            .user_passkeys(user)
            .await?
            .into_iter()
            .map(|(_, k)| k)
            .collect();
        if keys.is_empty() {
            return Err(ServiceError::NotFound);
        }
        let (rcr, state) = wa
            .start_passkey_authentication(&keys)
            .map_err(ServiceError::internal)?;
        let token = self
            .new_ceremony(
                Some(user),
                "authenticate",
                serde_json::to_value(&state).map_err(ServiceError::internal)?,
            )
            .await?;
        Ok(Ceremony {
            options: serde_json::to_value(&rcr).map_err(ServiceError::internal)?,
            token,
        })
    }

    /// Verifies the answer to [`Services::passkey_auth_start`] for `user`.
    pub async fn passkey_auth_finish(
        &self,
        user: UserId,
        ceremony: &str,
        credential: &str,
    ) -> Result<()> {
        let wa = self.webauthn()?;
        let (owner, state) = self.take_ceremony(ceremony, "authenticate").await?;
        if owner != Some(user) {
            return Err(invalid("ceremony of another user"));
        }
        let state: PasskeyAuthentication = serde_json::from_value(state).map_err(invalid)?;
        let cred: PublicKeyCredential = serde_json::from_str(credential).map_err(invalid)?;
        let result = wa
            .finish_passkey_authentication(&cred, &state)
            .map_err(invalid)?;
        self.record_passkey_use(user, &result).await
    }

    async fn record_passkey_use(
        &self,
        user: UserId,
        result: &webauthn_rs::prelude::AuthenticationResult,
    ) -> Result<()> {
        for (id, mut key) in self.user_passkeys(user).await? {
            if key.cred_id() == result.cred_id() {
                key.update_credential(result);
                slath_db::mfa::passkey_used(
                    &self.pool,
                    id,
                    &serde_json::to_value(&key).map_err(ServiceError::internal)?,
                )
                .await?;
            }
        }
        Ok(())
    }

    /// Starts a passwordless sign-in (any discoverable passkey).
    pub async fn passkey_discover_start(&self) -> Result<Ceremony> {
        let wa = self.webauthn()?;
        let (rcr, state) = wa
            .start_discoverable_authentication()
            .map_err(ServiceError::internal)?;
        let token = self
            .new_ceremony(
                None,
                "discover",
                serde_json::to_value(&state).map_err(ServiceError::internal)?,
            )
            .await?;
        Ok(Ceremony {
            options: serde_json::to_value(&rcr).map_err(ServiceError::internal)?,
            token,
        })
    }

    /// Finishes a passwordless sign-in: a session with auth level `passkey`.
    pub async fn passkey_login(
        &self,
        ceremony: &str,
        credential: &str,
        info: &NewSessionInfo,
    ) -> Result<(SessionCookie, UserId)> {
        let wa = self.webauthn()?;
        let (_, state) = self.take_ceremony(ceremony, "discover").await?;
        let state: DiscoverableAuthentication = serde_json::from_value(state).map_err(invalid)?;
        let cred: PublicKeyCredential = serde_json::from_str(credential).map_err(invalid)?;
        let (user_uuid, cred_id) = wa
            .identify_discoverable_authentication(&cred)
            .map_err(invalid)?;
        let user = UserId::from_uuid(user_uuid);
        let account = slath_db::users::get(&self.pool, user)
            .await?
            .filter(|u| u.state == UserState::Active && u.kind == UserKind::Human)
            .ok_or_else(|| invalid("unknown or inactive account"))?;
        let key = self
            .user_passkeys(user)
            .await?
            .into_iter()
            .map(|(_, k)| k)
            .find(|k| {
                let id: &[u8] = k.cred_id().as_ref();
                id == cred_id
            })
            .ok_or_else(|| invalid("unknown credential"))?;
        let result = wa
            .finish_discoverable_authentication_v2(&cred, state, &key)
            .map_err(invalid)?;
        self.record_passkey_use(user, &result).await?;
        let cookie = self.create_session(user, AuthLevel::Passkey, info).await?;
        let who = Subject::User {
            id: user,
            kind: account.kind,
            state: account.state,
            is_site_admin: account.is_admin,
            credential: slath_core::authz::Credential::Session,
        };
        self.audit(
            &who,
            "login.success",
            crate::audit::Target::new("user", Some(*user.as_uuid())),
            serde_json::json!({"first_factor": "passkey"}),
        )
        .await;
        Ok((cookie, user))
    }

    /// Completes a sign-in challenge (after the password) with a passkey.
    pub async fn complete_login_with_passkey(
        &self,
        challenge: &str,
        ceremony: &str,
        credential: &str,
        info: &NewSessionInfo,
    ) -> Result<(SessionCookie, UserId)> {
        let c = self
            .challenge(challenge)
            .await?
            .filter(|c| c.purpose == "login")
            .ok_or(ServiceError::Unauthenticated)?;
        self.passkey_auth_finish(c.user_id, ceremony, credential)
            .await?;
        slath_db::mfa::delete_challenge(&self.pool, &token::storage_hash(challenge)).await?;
        let cookie = self.create_session(c.user_id, AuthLevel::Mfa, info).await?;
        if let Some(u) = slath_db::users::get(&self.pool, c.user_id).await? {
            let who = Subject::User {
                id: u.id,
                kind: u.kind,
                state: u.state,
                is_site_admin: u.is_admin,
                credential: slath_core::authz::Credential::Session,
            };
            self.audit(
                &who,
                "login.success",
                crate::audit::Target::new("user", Some(*u.id.as_uuid())),
                serde_json::json!({"first_factor": c.first_factor, "second_factor": "passkey"}),
            )
            .await;
        }
        Ok((cookie, c.user_id))
    }

    /// Enters sudo mode with a passkey of the session's user.
    pub async fn sudo_with_passkey(
        &self,
        session: &Session,
        ceremony: &str,
        credential: &str,
    ) -> Result<()> {
        self.passkey_auth_finish(session.user_id(), ceremony, credential)
            .await?;
        slath_db::sessions::set_sudo(
            &self.pool,
            &session.row.id_hash,
            OffsetDateTime::now_utc() + crate::sessions::SUDO_DURATION,
        )
        .await?;
        self.audit(
            &session.subject,
            "sudo.enter",
            crate::audit::Target::new("user", Some(*session.user_id().as_uuid())),
            serde_json::json!({"with": "passkey"}),
        )
        .await;
        Ok(())
    }
}
