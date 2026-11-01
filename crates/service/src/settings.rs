// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Account settings of the signed-in user: profile, password, sessions,
//! and sudo mode.

use slath_auth::password;
use slath_core::Secret;
use time::OffsetDateTime;

use crate::Services;
use crate::accounts::{LoginError, hash_password};
use crate::error::{FieldError, Result, ServiceError};
use crate::sessions::{SUDO_DURATION, Session};

/// One entry of the sessions list.
#[derive(Debug, Clone)]
pub struct SessionListItem {
    /// Opaque identifier for revoking (hex of the stored hash; the cookie
    /// value can't be derived from it).
    pub id: String,
    pub current: bool,
    pub created_at: OffsetDateTime,
    pub last_seen_at: OffsetDateTime,
    pub ip: Option<String>,
    pub user_agent: Option<String>,
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn unhex(s: &str) -> Option<Vec<u8>> {
    if s.len() != 64 {
        return None;
    }
    (0..32)
        .map(|i| u8::from_str_radix(s.get(2 * i..2 * i + 2)?, 16).ok())
        .collect()
}

/// Maximum display name length in characters.
pub const MAX_DISPLAY_NAME: usize = 100;

impl Services {
    /// Fails with [`ServiceError::SudoRequired`] unless the session is in sudo mode.
    pub fn require_sudo(&self, session: &Session) -> Result<()> {
        if session.in_sudo() {
            Ok(())
        } else {
            Err(ServiceError::SudoRequired)
        }
    }

    pub async fn update_profile(&self, session: &Session, display_name: &str) -> Result<()> {
        let d = display_name.trim();
        if d.chars().count() > MAX_DISPLAY_NAME {
            return Err(ServiceError::invalid(
                "display_name",
                "too_long",
                format!("Use at most {MAX_DISPLAY_NAME} characters."),
            ));
        }
        if d.chars().any(|c| {
            c.is_control() || matches!(c, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
        }) {
            return Err(ServiceError::invalid(
                "display_name",
                "invalid_chars",
                "Remove control and text-direction characters.",
            ));
        }
        slath_db::users::update_profile(
            &self.pool,
            session.user_id(),
            (!d.is_empty()).then_some(d),
        )
        .await?;
        Ok(())
    }

    /// Changes the password (sudo mode) and signs out every other session.
    pub async fn change_password(
        &self,
        session: &Session,
        new: Secret<String>,
        confirm: Secret<String>,
    ) -> Result<u64> {
        self.require_sudo(session)?;
        let user = slath_db::users::get(&self.pool, session.user_id())
            .await?
            .ok_or(ServiceError::NotFound)?;
        let new = password::normalize(new.expose());
        let confirm = password::normalize(confirm.expose());
        let mut errors = Vec::new();
        if let Err(e) = password::check_policy(&new, &user.name, user.email.as_deref()) {
            errors.push(FieldError::new("new_password", "policy", e.to_string()));
        }
        if new.expose() != confirm.expose() {
            errors.push(FieldError::new(
                "confirm_password",
                "mismatch",
                "The passwords don't match.",
            ));
        }
        if !errors.is_empty() {
            return Err(ServiceError::Invalid(errors));
        }
        let hash = hash_password(new).await?;
        let mut tx = self.pool.begin().await.map_err(ServiceError::internal)?;
        slath_db::users::set_password_hash(&mut *tx, user.id, &hash).await?;
        let revoked =
            slath_db::sessions::delete_all_for_user(&mut *tx, user.id, Some(&session.row.id_hash))
                .await?;
        tx.commit().await.map_err(ServiceError::internal)?;
        Ok(revoked)
    }

    pub async fn list_sessions(&self, session: &Session) -> Result<Vec<SessionListItem>> {
        let idle = time::Duration::days(i64::from(self.config.auth.session_idle_days));
        let rows = slath_db::sessions::list_for_user(&self.pool, session.user_id(), idle).await?;
        Ok(rows
            .into_iter()
            .map(|r| SessionListItem {
                current: r.id_hash == session.row.id_hash,
                id: hex(&r.id_hash),
                created_at: r.created_at,
                last_seen_at: r.last_seen_at,
                ip: r.ip,
                user_agent: r.user_agent,
            })
            .collect())
    }

    /// Revokes one of the user's sessions. Other users' sessions are "not found".
    pub async fn revoke_session(&self, session: &Session, id: &str) -> Result<()> {
        let hash = unhex(id).ok_or(ServiceError::NotFound)?;
        if slath_db::sessions::delete_for_user(&self.pool, session.user_id(), &hash).await? {
            Ok(())
        } else {
            Err(ServiceError::NotFound)
        }
    }

    /// Signs out every session of the user, including this one.
    pub async fn sign_out_everywhere(&self, session: &Session) -> Result<u64> {
        Ok(slath_db::sessions::delete_all_for_user(&self.pool, session.user_id(), None).await?)
    }

    /// Re-authenticates with the password and starts sudo mode. Failures
    /// count toward the same throttle as sign-in.
    pub async fn reauthenticate(
        &self,
        session: &Session,
        pw: Secret<String>,
    ) -> Result<(), LoginError> {
        let key = format!("account:{}", session.row.user_name.to_lowercase());
        let failures = slath_db::sessions::failures(&self.pool, &key)
            .await
            .map_err(|e| LoginError::Internal(e.to_string()))?;
        if i64::from(failures) >= i64::from(self.config.limits.login_failures_per_account) {
            return Err(LoginError::Throttled);
        }
        let phc = slath_db::users::password_hash(&self.pool, session.user_id())
            .await
            .map_err(|e| LoginError::Internal(e.to_string()))?;
        let pw = password::normalize(pw.expose());
        let ok = tokio::task::spawn_blocking(move || match phc {
            Some(h) => password::verify(&pw, &h),
            None => {
                password::verify_dummy(&pw);
                false
            }
        })
        .await
        .map_err(|e| LoginError::Internal(e.to_string()))?;
        if !ok {
            slath_db::sessions::record_failures(&self.pool, &[key])
                .await
                .map_err(|e| LoginError::Internal(e.to_string()))?;
            return Err(LoginError::Invalid);
        }
        slath_db::sessions::set_sudo(
            &self.pool,
            &session.row.id_hash,
            OffsetDateTime::now_utc() + SUDO_DURATION,
        )
        .await
        .map_err(|e| LoginError::Internal(e.to_string()))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_ids_round_trip() {
        let h = [7u8; 32];
        assert_eq!(unhex(&hex(&h)), Some(h.to_vec()));
        assert_eq!(unhex("zz"), None);
        assert_eq!(unhex(&"g".repeat(64)), None);
    }
}
