// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Emailed single-use links: password reset and email verification.
//!
//! - A reset is requested by email address. The answer is the same whether
//!   or not an account has that (verified) address, and the email is sent
//!   from a background task so timing doesn't tell either. Tokens are 32
//!   random bytes, stored as SHA-256, valid for 30 minutes, and single-use.
//! - A reset doesn't bypass two-factor authentication: users with a second
//!   factor enter a code with the new password. A successful reset signs
//!   out every session.
//! - Changing the address makes it unverified until the link sent to it is
//!   used. Resets go only to verified addresses.

use slath_auth::{password, token};
use slath_core::Secret;
use slath_core::authz::Subject;
use slath_core::ids::UserId;
use time::OffsetDateTime;

use crate::Services;
use crate::accounts::hash_password;
use crate::email::Email;
use crate::error::{FieldError, Result, ServiceError};
use crate::sessions::Session;

const RESET_TTL: time::Duration = time::Duration::minutes(30);
const VERIFY_TTL: time::Duration = time::Duration::hours(48);
/// Reset emails per account per hour.
const RESETS_PER_HOUR: i64 = 3;
/// Reset requests per IP per 15 minutes.
const RESET_REQUESTS_PER_IP: i32 = 20;

/// What the reset form needs to know about a valid token.
#[derive(Debug, Clone)]
pub struct ResetInfo {
    pub username: String,
    /// The account has a second factor; the form asks for a code.
    pub needs_code: bool,
}

/// Result of following a verification link.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verified {
    Done,
    /// Expired, used, or for an address the account no longer has.
    Stale,
    /// Another account verified the address first.
    Taken,
}

fn new_token() -> Result<(Secret<String>, [u8; 32])> {
    let t = token::new_session_id().map_err(ServiceError::internal)?;
    let h = token::storage_hash(t.expose());
    Ok((t, h))
}

fn token_hash(t: &str) -> Option<[u8; 32]> {
    token::is_session_id(t).then(|| token::storage_hash(t))
}

impl Services {
    fn link(&self, path_and_query: &str) -> String {
        self.config.server.public_url.join(path_and_query)
    }

    /// Requests a reset link for `email`. Always succeeds the same way.
    pub async fn request_password_reset(&self, email: &str, ip: Option<&str>) -> Result<()> {
        if !self.email_enabled() {
            return Err(ServiceError::NotFound);
        }
        let email = email.trim().to_owned();
        if email.is_empty() || email.len() > 254 || email.contains(['\r', '\n']) {
            return Err(ServiceError::invalid(
                "email",
                "invalid",
                "Enter the email address of your account.",
            ));
        }
        if let Some(ip) = ip {
            let key = format!("reset-ip:{ip}");
            let n = slath_db::sessions::record_failures(&self.pool, &[key]).await?;
            if n.first().is_some_and(|n| *n > RESET_REQUESTS_PER_IP) {
                return Err(ServiceError::RateLimited { retry_after: 900 });
            }
        }
        // The rest runs detached, so the answer takes the same time for
        // known and unknown addresses.
        let svc = self.clone();
        tokio::spawn(async move {
            if let Err(e) = svc.send_reset(&email).await {
                tracing::warn!(error = %e, "sending a password reset failed");
            }
        });
        Ok(())
    }

    async fn send_reset(&self, email: &str) -> Result<()> {
        let Some(user) = slath_db::email_tokens::user_by_verified_email(&self.pool, email).await?
        else {
            return Ok(());
        };
        let hour_ago = OffsetDateTime::now_utc() - time::Duration::hours(1);
        if slath_db::email_tokens::recent_resets(&self.pool, user, hour_ago).await?
            >= RESETS_PER_HOUR
        {
            return Ok(());
        }
        let (t, h) = new_token()?;
        slath_db::email_tokens::insert_reset(
            &self.pool,
            &h,
            user,
            OffsetDateTime::now_utc() + RESET_TTL,
        )
        .await?;
        let link = self.link(&format!("/-/password/reset/confirm?token={}", t.expose()));
        let name = slath_db::users::get(&self.pool, user)
            .await?
            .map(|u| u.name)
            .unwrap_or_default();
        self.queue_email(&Email {
            // The stored address, never the typed one.
            to: email_of(self, user).await?.unwrap_or_default(),
            subject: "Reset your Slath password".to_owned(),
            text: format!(
                "Hello {name},\n\nSomeone asked to reset the password of your account at {}.\n\
                 To choose a new password, open this link within 30 minutes:\n\n{link}\n\n\
                 If it wasn't you, ignore this email; your password stays the same.\n",
                self.config.server.public_url.origin()
            ),
            ..Email::default()
        })
        .await?;
        self.audit(
            &Subject::System,
            "password.reset_request",
            crate::audit::Target::new("user", Some(*user.as_uuid())),
            serde_json::json!({}),
        )
        .await;
        Ok(())
    }

    /// The account a live reset token belongs to.
    pub async fn reset_info(&self, t: &str) -> Result<Option<ResetInfo>> {
        let Some(h) = token_hash(t) else {
            return Ok(None);
        };
        let Some(user) = slath_db::email_tokens::reset_user(&self.pool, &h).await? else {
            return Ok(None);
        };
        let Some(u) = slath_db::users::get(&self.pool, user).await? else {
            return Ok(None);
        };
        Ok(Some(ResetInfo {
            username: u.name,
            needs_code: self.has_second_factor(user).await?,
        }))
    }

    /// Sets a new password with a reset token. Users with a second factor
    /// must give a valid code. Signs out every session.
    pub async fn reset_password(
        &self,
        t: &str,
        new: Secret<String>,
        confirm: Secret<String>,
        code: &str,
    ) -> Result<()> {
        let stale = || {
            ServiceError::invalid(
                "token",
                "expired",
                "This link has expired or was already used. Request a new one.",
            )
        };
        let h = token_hash(t).ok_or_else(stale)?;
        let user_id = slath_db::email_tokens::reset_user(&self.pool, &h)
            .await?
            .ok_or_else(stale)?;
        let user = slath_db::users::get(&self.pool, user_id)
            .await?
            .ok_or_else(stale)?;
        let new = password::normalize(new.expose());
        let confirm = password::normalize(confirm.expose());
        let mut errors: Vec<FieldError> = Vec::new();
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
        if self.has_second_factor(user_id).await? {
            let key = format!("account:{}", user.name.to_lowercase());
            let failures = slath_db::sessions::failures(&self.pool, &key).await?;
            if i64::from(failures) >= i64::from(self.config.limits.login_failures_per_account) {
                return Err(ServiceError::RateLimited { retry_after: 900 });
            }
            if self.check_second_factor(user_id, code).await?.is_none() {
                slath_db::sessions::record_failures(&self.pool, &[key]).await?;
                return Err(ServiceError::invalid(
                    "code",
                    "invalid",
                    "Enter the current code from your authenticator app, or a recovery code.",
                ));
            }
        }
        let hash = hash_password(new).await?;
        let mut tx = self.pool.begin().await.map_err(ServiceError::internal)?;
        if slath_db::email_tokens::use_reset(&mut tx, &h).await? != Some(user_id) {
            return Err(stale());
        }
        slath_db::users::set_password_hash(&mut *tx, user_id, &hash).await?;
        slath_db::sessions::delete_all_for_user(&mut *tx, user_id, None).await?;
        tx.commit().await.map_err(ServiceError::internal)?;
        self.audit(
            &Subject::System,
            "password.reset",
            crate::audit::Target::new("user", Some(*user_id.as_uuid())),
            serde_json::json!({}),
        )
        .await;
        self.notify_security_change(
            user_id,
            "The password of your account was reset, and all sessions were signed out.",
        )
        .await;
        Ok(())
    }

    /// Sends a verification link to the user's current address (if email
    /// is on and the address isn't verified yet). Returns whether it did.
    pub async fn send_verification(&self, user: UserId) -> Result<bool> {
        if !self.email_enabled() {
            return Ok(false);
        }
        let (Some(email), false) = slath_db::email_tokens::email_status(&self.pool, user).await?
        else {
            return Ok(false);
        };
        let (t, h) = new_token()?;
        slath_db::email_tokens::insert_verification(
            &self.pool,
            &h,
            user,
            &email,
            OffsetDateTime::now_utc() + VERIFY_TTL,
        )
        .await?;
        let link = self.link(&format!("/-/verify-email?token={}", t.expose()));
        self.queue_email(&Email {
            to: email,
            subject: "Confirm your email address for Slath".to_owned(),
            text: format!(
                "Confirm that this address belongs to your account at {} by opening this link \
                 within 48 hours:\n\n{link}\n\nIf you didn't add it, ignore this email.\n",
                self.config.server.public_url.origin()
            ),
            ..Email::default()
        })
        .await
    }

    /// The address a live verification link confirms.
    pub async fn verification_email(&self, t: &str) -> Result<Option<String>> {
        let Some(h) = token_hash(t) else {
            return Ok(None);
        };
        Ok(slath_db::email_tokens::verification(&self.pool, &h)
            .await?
            .map(|(_, e)| e))
    }

    /// Follows a verification link.
    pub async fn verify_email(&self, t: &str) -> Result<Verified> {
        let Some(h) = token_hash(t) else {
            return Ok(Verified::Stale);
        };
        let mut tx = self.pool.begin().await.map_err(ServiceError::internal)?;
        let r = slath_db::email_tokens::use_verification(&mut tx, &h).await;
        match r {
            Ok(true) => {
                tx.commit().await.map_err(ServiceError::internal)?;
                Ok(Verified::Done)
            }
            Ok(false) => Ok(Verified::Stale),
            Err(slath_db::DbError::Conflict(_)) => Ok(Verified::Taken),
            Err(e) => Err(e.into()),
        }
    }

    /// Changes the user's own address (unverified until confirmed).
    pub async fn change_email(&self, session: &Session, email: &str) -> Result<()> {
        let email = email.trim();
        if !email.is_empty() && !crate::accounts::valid_email(email) {
            return Err(ServiceError::invalid(
                "email",
                "invalid",
                "Enter an email address like name@example.org.",
            ));
        }
        let user = session.user_id();
        let (current, _) = slath_db::email_tokens::email_status(&self.pool, user).await?;
        if current
            .as_deref()
            .is_some_and(|c| c.eq_ignore_ascii_case(email))
        {
            return Ok(());
        }
        let old = current.clone();
        slath_db::email_tokens::set_email(&self.pool, user, (!email.is_empty()).then_some(email))
            .await?;
        self.audit(
            &session.subject,
            "user.email_change",
            crate::audit::Target::new("user", Some(*user.as_uuid())),
            serde_json::json!({}),
        )
        .await;
        // Tell the old address, then confirm the new one.
        if let Some(old) = old {
            let _ = self
                .queue_email(&Email {
                    to: old,
                    subject: "Your Slath email address was changed".to_owned(),
                    text: format!(
                        "The email address of your account at {} was changed. If you didn't \
                         do this, sign in, check your security settings, and contact an \
                         administrator.\n",
                        self.config.server.public_url.origin()
                    ),
                    ..Email::default()
                })
                .await;
        }
        if !email.is_empty() {
            self.send_verification(user).await?;
        }
        Ok(())
    }
}

async fn email_of(svc: &Services, user: UserId) -> Result<Option<String>> {
    Ok(slath_db::email_tokens::email_status(&svc.pool, user)
        .await?
        .0)
}
