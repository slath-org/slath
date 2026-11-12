// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Browser sessions: creation, lookup, and revocation.

use slath_auth::token;
use slath_core::Secret;
use slath_core::authz::{Credential, Subject, UserKind, UserState};
use slath_core::ids::UserId;
use slath_db::sessions::{AuthLevel, SessionRow};
use time::OffsetDateTime;

use crate::Services;
use crate::error::{Result, ServiceError};

/// Request facts stored with a new session.
#[derive(Debug, Clone, Default)]
pub struct NewSessionInfo {
    pub ip: Option<String>,
    pub user_agent: Option<String>,
}

/// The value to put in the session cookie. Shown to the browser only.
pub struct SessionCookie {
    pub value: Secret<String>,
    pub expires_at: OffsetDateTime,
}

/// A resolved session.
#[derive(Debug, Clone)]
pub struct Session {
    pub row: SessionRow,
    pub subject: Subject,
}

impl Session {
    #[must_use]
    pub fn user_id(&self) -> UserId {
        self.row.user_id
    }

    /// True while the session is in sudo mode.
    #[must_use]
    pub fn in_sudo(&self) -> bool {
        self.row
            .sudo_until
            .is_some_and(|t| t > OffsetDateTime::now_utc())
    }
}

/// How long sudo mode lasts after re-authenticating.
pub const SUDO_DURATION: time::Duration = time::Duration::minutes(15);

impl Services {
    fn idle(&self) -> time::Duration {
        time::Duration::days(i64::from(self.config.auth.session_idle_days))
    }

    /// Creates a session with a fresh random ID. Signing in always creates a
    /// new ID (session fixation defense); sign-in also starts sudo mode.
    pub(crate) async fn create_session(
        &self,
        user: UserId,
        level: AuthLevel,
        info: &NewSessionInfo,
    ) -> Result<SessionCookie> {
        let value = token::new_session_id().map_err(ServiceError::internal)?;
        let hash = token::storage_hash(value.expose());
        let now = OffsetDateTime::now_utc();
        let expires_at =
            now + time::Duration::days(i64::from(self.config.auth.session_absolute_days));
        slath_db::sessions::insert(
            &self.pool,
            &slath_db::sessions::NewSession {
                id_hash: &hash,
                user_id: user,
                expires_at,
                auth_level: level,
                sudo_until: Some(now + SUDO_DURATION),
                ip: info.ip.as_deref(),
                user_agent: info.user_agent.as_deref(),
            },
        )
        .await?;
        Ok(SessionCookie { value, expires_at })
    }

    /// Resolves a session cookie. Expired, revoked, and malformed values and
    /// sessions of users who are no longer active resolve to `None`.
    pub async fn resolve_session(&self, cookie: &str) -> Result<Option<Session>> {
        if !token::is_session_id(cookie) {
            return Ok(None);
        }
        let hash = token::storage_hash(cookie);
        let Some(row) = slath_db::sessions::find(&self.pool, &hash, self.idle()).await? else {
            return Ok(None);
        };
        // Bots never have sessions; refuse one should it exist anyway.
        if row.user_state != UserState::Active || row.user_kind != UserKind::Human {
            return Ok(None);
        }
        if OffsetDateTime::now_utc() - row.last_seen_at > time::Duration::minutes(1) {
            slath_db::sessions::touch(&self.pool, &row.id_hash).await?;
        }
        let is_site_admin = row.is_admin
            && self
                .admin_mfa_satisfied(row.user_id, Some(row.auth_level))
                .await?;
        let subject = Subject::User {
            id: row.user_id,
            kind: row.user_kind,
            state: row.user_state,
            is_site_admin,
            credential: Credential::Session,
        };
        Ok(Some(Session { row, subject }))
    }

    /// With `auth.require_mfa_for_admins`, administrator powers need a
    /// second factor: sessions must have signed in with one (`level`), and
    /// tokens and SSH keys (`None`) must belong to an account that has one.
    /// Without, an administrator acts as a regular user until they set it up.
    pub async fn admin_mfa_satisfied(
        &self,
        user: UserId,
        level: Option<AuthLevel>,
    ) -> Result<bool> {
        if !self.config.auth.require_mfa_for_admins {
            return Ok(true);
        }
        Ok(match level {
            Some(AuthLevel::Mfa | AuthLevel::Passkey) => true,
            Some(_) => false,
            None => self.has_second_factor(user).await?,
        })
    }

    /// True if the session's user is an administrator who can't use
    /// administration until they sign in with a second factor.
    #[must_use]
    pub fn admin_needs_mfa(&self, session: &Session) -> bool {
        session.row.is_admin
            && !matches!(
                session.subject,
                Subject::User {
                    is_site_admin: true,
                    ..
                }
            )
    }

    /// Ends a session (logout).
    pub async fn logout(&self, cookie: &str) -> Result<()> {
        if token::is_session_id(cookie) {
            slath_db::sessions::delete(&self.pool, &token::storage_hash(cookie)).await?;
        }
        Ok(())
    }
}
