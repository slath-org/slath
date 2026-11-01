// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Browser sessions: creation, lookup, and revocation.

use slath_auth::token;
use slath_core::Secret;
use slath_core::authz::{Credential, Subject, UserState};
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
        if row.user_state != UserState::Active {
            return Ok(None);
        }
        if OffsetDateTime::now_utc() - row.last_seen_at > time::Duration::minutes(1) {
            slath_db::sessions::touch(&self.pool, &row.id_hash).await?;
        }
        let subject = Subject::User {
            id: row.user_id,
            kind: row.user_kind,
            state: row.user_state,
            is_site_admin: row.is_admin,
            credential: Credential::Session,
        };
        Ok(Some(Session { row, subject }))
    }

    /// Ends a session (logout).
    pub async fn logout(&self, cookie: &str) -> Result<()> {
        if token::is_session_id(cookie) {
            slath_db::sessions::delete(&self.pool, &token::storage_hash(cookie)).await?;
        }
        Ok(())
    }
}
