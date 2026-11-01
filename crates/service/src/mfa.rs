// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Two-factor authentication: TOTP enrollment and removal, recovery codes,
//! the second sign-in step, and the administrator reset.
//!
//! A sign-in that passed its first factor (password, a password reset, or
//! an OIDC login that doesn't count as MFA) of a user with a second factor
//! becomes a *challenge*: a random token in a short-lived cookie, stored
//! hashed with the user and purpose. Only a correct code turns it into a
//! session. Wrong codes count; five end the challenge.

use base64::Engine;
use slath_auth::{password, token, totp};
use slath_core::Secret;
use slath_core::authz::{Subject, UserState};
use slath_core::ids::UserId;
use slath_db::sessions::AuthLevel;
use time::OffsetDateTime;

use crate::Services;
use crate::accounts::LoginError;
use crate::error::{Result, ServiceError};
use crate::sessions::{NewSessionInfo, Session, SessionCookie};

/// How long a second-factor step may take.
const CHALLENGE_TTL: time::Duration = time::Duration::minutes(10);
/// Wrong codes before a challenge ends.
const CHALLENGE_TRIES: i32 = 5;

/// What the security settings page shows.
#[derive(Debug, Clone, Default)]
pub struct MfaStatus {
    pub totp: bool,
    /// A setup was started but not confirmed.
    pub totp_pending: bool,
    pub recovery_codes_left: usize,
}

/// What enrollment shows: the seed for manual entry and as a QR code.
pub struct TotpSetup {
    /// Base32, in groups of four for reading.
    pub secret: String,
    pub uri: Secret<String>,
    /// `data:image/svg+xml;base64,…` of the QR code.
    pub qr: String,
}

/// Which factor a code matched.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Factor {
    Totp,
    RecoveryCode,
}

/// A started second-factor step.
pub struct Challenge {
    pub token: Secret<String>,
    pub expires_at: OffsetDateTime,
}

/// Outcome of a first factor.
pub enum SignIn {
    Session(SessionCookie, UserId),
    /// The user has a second factor: show the code step.
    SecondFactor(Challenge),
}

fn qr_data_uri(text: &str) -> Result<String> {
    let code = qrcode::QrCode::new(text.as_bytes()).map_err(ServiceError::internal)?;
    let svg = code
        .render::<qrcode::render::svg::Color<'_>>()
        .min_dimensions(200, 200)
        .dark_color(qrcode::render::svg::Color("#000000"))
        .light_color(qrcode::render::svg::Color("#ffffff"))
        .build();
    Ok(format!(
        "data:image/svg+xml;base64,{}",
        base64::engine::general_purpose::STANDARD.encode(svg)
    ))
}

fn grouped(s: &str) -> String {
    s.as_bytes()
        .chunks(4)
        .map(|c| String::from_utf8_lossy(c).into_owned())
        .collect::<Vec<_>>()
        .join(" ")
}

impl Services {
    fn totp_secret(&self, user: UserId, t: &slath_db::mfa::Totp) -> Result<Secret<Vec<u8>>> {
        self.open(
            &t.secret_enc,
            &t.key_id,
            "totp_credentials",
            "secret_enc",
            &user.to_string(),
        )
    }

    /// The user's second factors.
    pub async fn mfa_status(&self, user: UserId) -> Result<MfaStatus> {
        let t = slath_db::mfa::totp(&self.pool, user).await?;
        let codes = slath_db::mfa::unused_recovery_codes(&self.pool, user).await?;
        Ok(MfaStatus {
            totp: t.as_ref().is_some_and(|t| t.confirmed),
            totp_pending: t.as_ref().is_some_and(|t| !t.confirmed),
            recovery_codes_left: codes.len(),
        })
    }

    /// True if the user has a confirmed second factor.
    pub async fn has_second_factor(&self, user: UserId) -> Result<bool> {
        Ok(slath_db::mfa::has_second_factor(&self.pool, user).await?)
    }

    /// Starts (or restarts) TOTP enrollment. Sudo mode.
    pub async fn totp_begin(&self, session: &Session) -> Result<TotpSetup> {
        self.require_sudo(session)?;
        let user = session.user_id();
        let secret = totp::new_secret().map_err(ServiceError::internal)?;
        let sealed = self
            .seal(
                secret.expose(),
                "totp_credentials",
                "secret_enc",
                &user.to_string(),
            )
            .await?;
        slath_db::mfa::put_pending_totp(&self.pool, user, &sealed.ciphertext, &sealed.key_id)
            .await
            .map_err(|e| match e {
                slath_db::DbError::Conflict(_) => {
                    ServiceError::Conflict("two-factor authentication is already set up".to_owned())
                }
                other => other.into(),
            })?;
        self.totp_setup(session, &secret)
    }

    /// The setup data of a pending enrollment (to show the page again).
    pub async fn totp_pending(&self, session: &Session) -> Result<Option<TotpSetup>> {
        let user = session.user_id();
        match slath_db::mfa::totp(&self.pool, user).await? {
            Some(t) if !t.confirmed => {
                let secret = self.totp_secret(user, &t)?;
                self.totp_setup(session, &secret).map(Some)
            }
            _ => Ok(None),
        }
    }

    fn totp_setup(&self, session: &Session, secret: &Secret<Vec<u8>>) -> Result<TotpSetup> {
        let issuer = format!("Slath {}", self.config.server.public_url.host());
        let uri = totp::otpauth_uri(secret.expose(), &issuer, &session.row.user_name);
        Ok(TotpSetup {
            secret: grouped(&totp::base32(secret.expose())),
            qr: qr_data_uri(&uri)?,
            uri: Secret::new(uri),
        })
    }

    /// Confirms enrollment with a code from the app. Returns the new
    /// recovery codes (shown once).
    pub async fn totp_confirm(&self, session: &Session, code: &str) -> Result<Vec<Secret<String>>> {
        self.require_sudo(session)?;
        let user = session.user_id();
        let t = slath_db::mfa::totp(&self.pool, user)
            .await?
            .filter(|t| !t.confirmed)
            .ok_or_else(|| ServiceError::Conflict("no setup in progress".to_owned()))?;
        let secret = self.totp_secret(user, &t)?;
        let now = OffsetDateTime::now_utc().unix_timestamp();
        let wrong = || {
            ServiceError::invalid(
                "code",
                "invalid",
                "That code isn't right. Check the time on your device and enter the current code.",
            )
        };
        let step = totp::verify(secret.expose(), code, now, t.last_used_step).ok_or_else(wrong)?;
        if !slath_db::mfa::use_step(&self.pool, user, step, true).await? {
            return Err(wrong());
        }
        let codes = self.issue_recovery_codes(user).await?;
        self.audit(
            &session.subject,
            "mfa.totp_enable",
            crate::audit::Target::new("user", Some(*user.as_uuid())),
            serde_json::json!({}),
        )
        .await;
        self.notify_security_change(
            user,
            "Two-factor authentication was turned on for your account.",
        )
        .await;
        Ok(codes)
    }

    async fn issue_recovery_codes(&self, user: UserId) -> Result<Vec<Secret<String>>> {
        let codes = totp::new_recovery_codes().map_err(ServiceError::internal)?;
        let mut hashes = Vec::with_capacity(codes.len());
        for c in &codes {
            hashes.push(crate::accounts::hash_password(Secret::new(c.expose().clone())).await?);
        }
        let mut tx = self.pool.begin().await.map_err(ServiceError::internal)?;
        slath_db::mfa::replace_recovery_codes(&mut tx, user, &hashes).await?;
        tx.commit().await.map_err(ServiceError::internal)?;
        Ok(codes)
    }

    /// New recovery codes, replacing the old ones. Sudo mode.
    pub async fn regenerate_recovery_codes(
        &self,
        session: &Session,
    ) -> Result<Vec<Secret<String>>> {
        self.require_sudo(session)?;
        let user = session.user_id();
        if !self.has_second_factor(user).await? {
            return Err(ServiceError::Conflict(
                "set up two-factor authentication first".to_owned(),
            ));
        }
        let codes = self.issue_recovery_codes(user).await?;
        self.audit(
            &session.subject,
            "mfa.recovery_codes_regenerate",
            crate::audit::Target::new("user", Some(*user.as_uuid())),
            serde_json::json!({}),
        )
        .await;
        Ok(codes)
    }

    /// Turns TOTP off and deletes the recovery codes. Sudo mode; notifies
    /// the user by email.
    pub async fn totp_disable(&self, session: &Session) -> Result<()> {
        self.require_sudo(session)?;
        let user = session.user_id();
        if !slath_db::mfa::delete_totp(&self.pool, user).await? {
            return Err(ServiceError::NotFound);
        }
        slath_db::mfa::delete_recovery_codes(&self.pool, user).await?;
        self.audit(
            &session.subject,
            "mfa.totp_disable",
            crate::audit::Target::new("user", Some(*user.as_uuid())),
            serde_json::json!({}),
        )
        .await;
        self.notify_security_change(
            user,
            "Two-factor authentication was turned off for your account. If you didn't do this, \
             change your password and contact an administrator.",
        )
        .await;
        Ok(())
    }

    /// Checks a TOTP code or a recovery code of `user`. A matching code is
    /// used up (TOTP: its step; recovery code: the code), so it can't be
    /// used again.
    pub async fn check_second_factor(&self, user: UserId, code: &str) -> Result<Option<Factor>> {
        if totp::normalize(code).is_some() {
            let Some(t) = slath_db::mfa::totp(&self.pool, user)
                .await?
                .filter(|t| t.confirmed)
            else {
                return Ok(None);
            };
            let secret = self.totp_secret(user, &t)?;
            let now = OffsetDateTime::now_utc().unix_timestamp();
            return Ok(
                match totp::verify(secret.expose(), code, now, t.last_used_step) {
                    Some(step)
                        if slath_db::mfa::use_step(&self.pool, user, step, false).await? =>
                    {
                        Some(Factor::Totp)
                    }
                    _ => None,
                },
            );
        }
        let Some(code) = totp::normalize_recovery_code(code) else {
            return Ok(None);
        };
        let candidates = slath_db::mfa::unused_recovery_codes(&self.pool, user).await?;
        let found = tokio::task::spawn_blocking(move || {
            let code = Secret::new(code);
            candidates
                .into_iter()
                .find(|(_, h)| password::verify(&code, h))
                .map(|(id, _)| id)
        })
        .await
        .map_err(ServiceError::internal)?;
        match found {
            Some(id) if slath_db::mfa::use_recovery_code(&self.pool, id).await? => {
                Ok(Some(Factor::RecoveryCode))
            }
            _ => Ok(None),
        }
    }

    /// Starts a second-factor step for a user who passed `first_factor`.
    pub async fn start_challenge(
        &self,
        user: UserId,
        purpose: &str,
        first_factor: &str,
        data: serde_json::Value,
    ) -> Result<Challenge> {
        let value = token::new_session_id().map_err(ServiceError::internal)?;
        let hash = token::storage_hash(value.expose());
        let expires_at = OffsetDateTime::now_utc() + CHALLENGE_TTL;
        slath_db::mfa::insert_challenge(
            &self.pool,
            &slath_db::mfa::NewChallenge {
                id_hash: &hash,
                user_id: user,
                purpose,
                first_factor,
                data,
                expires_at,
            },
        )
        .await?;
        Ok(Challenge {
            token: value,
            expires_at,
        })
    }

    /// The live challenge behind a cookie value.
    pub async fn challenge(&self, token_value: &str) -> Result<Option<slath_db::mfa::Challenge>> {
        if !token::is_session_id(token_value) {
            return Ok(None);
        }
        Ok(slath_db::mfa::challenge(&self.pool, &token::storage_hash(token_value)).await?)
    }

    /// Completes a sign-in challenge with a code: creates the session
    /// (auth level `mfa`). Wrong codes count against the challenge and the
    /// account's sign-in throttle.
    pub async fn complete_login(
        &self,
        token_value: &str,
        code: &str,
        info: &NewSessionInfo,
    ) -> std::result::Result<(SessionCookie, UserId), LoginError> {
        let internal = |e: ServiceError| LoginError::Internal(e.to_string());
        let c = self
            .challenge(token_value)
            .await
            .map_err(internal)?
            .filter(|c| c.purpose == "login")
            .ok_or(LoginError::Expired)?;
        let hash = token::storage_hash(token_value);
        let user = slath_db::users::get(&self.pool, c.user_id)
            .await
            .map_err(|e| LoginError::Internal(e.to_string()))?
            .filter(|u| u.state == UserState::Active)
            .ok_or(LoginError::Expired)?;
        let account_key = format!("account:{}", user.name.to_lowercase());
        let failures = slath_db::sessions::failures(&self.pool, &account_key)
            .await
            .map_err(|e| LoginError::Internal(e.to_string()))?;
        if i64::from(failures) >= i64::from(self.config.limits.login_failures_per_account) {
            return Err(LoginError::Throttled);
        }
        match self
            .check_second_factor(c.user_id, code)
            .await
            .map_err(internal)?
        {
            Some(factor) => {
                slath_db::mfa::delete_challenge(&self.pool, &hash)
                    .await
                    .map_err(|e| LoginError::Internal(e.to_string()))?;
                let cookie = self
                    .create_session(c.user_id, AuthLevel::Mfa, info)
                    .await
                    .map_err(internal)?;
                let who = Subject::User {
                    id: user.id,
                    kind: user.kind,
                    state: user.state,
                    is_site_admin: user.is_admin,
                    credential: slath_core::authz::Credential::Session,
                };
                self.audit(
                    &who,
                    "login.success",
                    crate::audit::Target::new("user", Some(*user.id.as_uuid())),
                    serde_json::json!({
                        "first_factor": c.first_factor,
                        "second_factor": match factor {
                            Factor::Totp => "totp",
                            Factor::RecoveryCode => "recovery_code",
                        },
                    }),
                )
                .await;
                if factor == Factor::RecoveryCode {
                    self.notify_security_change(
                        user.id,
                        "A recovery code was used to sign in to your account.",
                    )
                    .await;
                }
                Ok((cookie, c.user_id))
            }
            None => {
                let n = slath_db::mfa::challenge_failed(&self.pool, &hash)
                    .await
                    .map_err(|e| LoginError::Internal(e.to_string()))?;
                slath_db::sessions::record_failures(&self.pool, &[account_key])
                    .await
                    .map_err(|e| LoginError::Internal(e.to_string()))?;
                if n >= CHALLENGE_TRIES {
                    let _ = slath_db::mfa::delete_challenge(&self.pool, &hash).await;
                    return Err(LoginError::Expired);
                }
                Err(LoginError::Invalid)
            }
        }
    }

    /// Ends a challenge (the user went back to the sign-in form).
    pub async fn cancel_challenge(&self, token_value: &str) -> Result<()> {
        if token::is_session_id(token_value) {
            slath_db::mfa::delete_challenge(&self.pool, &token::storage_hash(token_value)).await?;
        }
        Ok(())
    }

    /// `slath admin reset-mfa`: removes every second factor of a user
    /// (lost device), signs them out, and notifies them. Audited.
    pub async fn reset_mfa(&self, subject: &Subject, username: &str) -> Result<()> {
        match subject {
            Subject::System => {}
            s => self.require_site_admin(s)?,
        }
        let user = slath_db::users::get_by_name(&self.pool, username)
            .await?
            .ok_or(ServiceError::NotFound)?;
        slath_db::mfa::delete_totp(&self.pool, user.id).await?;
        slath_db::mfa::delete_recovery_codes(&self.pool, user.id).await?;
        slath_db::mfa::delete_passkeys(&self.pool, user.id).await?;
        slath_db::sessions::delete_all_for_user(&self.pool, user.id, None).await?;
        self.audit(
            subject,
            "mfa.reset",
            crate::audit::Target::new("user", Some(*user.id.as_uuid())),
            serde_json::json!({"username": user.name}),
        )
        .await;
        self.notify_security_change(
            user.id,
            "An administrator removed the two-factor authentication of your account. Sign in \
             with your password and set it up again.",
        )
        .await;
        Ok(())
    }

    /// Emails the user about a change to their account's security, if
    /// email is on and they have an address. Failures are logged only.
    pub(crate) async fn notify_security_change(&self, user: UserId, what: &str) {
        let Ok(Some(u)) = slath_db::users::get(&self.pool, user).await else {
            return;
        };
        let Some(to) = u.email else { return };
        let email = crate::email::Email {
            to: to.to_string(),
            subject: "Security change on your Slath account".to_owned(),
            text: format!(
                "Hello {},\n\n{what}\n\nTime: {} UTC\nInstance: {}\n",
                u.name,
                OffsetDateTime::now_utc()
                    .replace_nanosecond(0)
                    .unwrap_or_else(|_| OffsetDateTime::now_utc()),
                self.config.server.public_url.origin()
            ),
            ..crate::email::Email::default()
        };
        if let Err(e) = self.queue_email(&email).await {
            tracing::warn!(error = %e, "queueing a security notice failed");
        }
    }
}
