// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Self-service registration (`auth.registration`) and invitations.
//!
//! - `disabled`: only administrators create accounts; registration doesn't
//!   exist (404).
//! - `invite`: an invitation link from an administrator is needed. An
//!   invitation can be bound to an address, which then counts as confirmed.
//! - `open`: anyone may register. With email configured, an account whose
//!   address isn't confirmed can't create repositories or organizations
//!   (or, later, comment) until it is.

use slath_auth::token;
use slath_core::Secret;
use slath_core::authz::{Subject, UserState};
use slath_core::config::Registration;
use slath_core::ids::UserId;
use time::OffsetDateTime;

use crate::Services;
use crate::accounts::{NewAccount, check_account, insert_account};
use crate::error::{FieldError, Result, ServiceError};
use crate::sessions::{NewSessionInfo, SessionCookie};

/// Registrations per IP per 15 minutes.
const REGISTRATIONS_PER_IP: i32 = 5;
/// Default validity of an invitation.
pub const INVITE_DAYS: i64 = 7;

/// What the registration form submits.
pub struct RegisterInput {
    pub username: String,
    pub email: String,
    pub password: Secret<String>,
    pub confirm: Secret<String>,
    pub invite: Option<String>,
}

/// Whether and how someone may register.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Gate {
    Open,
    /// A live invitation; with the address it is bound to.
    Invited(Option<String>),
}

impl Services {
    /// Who may register right now (with `invite`, a token from a link).
    /// `NotFound` when registration isn't possible.
    pub async fn registration_gate(&self, invite: Option<&str>) -> Result<Gate> {
        match self.config.auth.registration {
            Registration::Disabled => Err(ServiceError::NotFound),
            Registration::Open if invite.is_none() => Ok(Gate::Open),
            Registration::Open | Registration::Invite => {
                let t = invite
                    .filter(|t| token::is_session_id(t))
                    .ok_or(ServiceError::NotFound)?;
                slath_db::invitations::live(&self.pool, &token::storage_hash(t))
                    .await?
                    .map(Gate::Invited)
                    .ok_or(ServiceError::NotFound)
            }
        }
    }

    /// Creates an account through the registration form and signs it in.
    pub async fn register(
        &self,
        r: RegisterInput,
        ip: Option<&str>,
        info: &NewSessionInfo,
    ) -> Result<(SessionCookie, UserId)> {
        let gate = self.registration_gate(r.invite.as_deref()).await?;
        if let Some(ip) = ip {
            let n = slath_db::sessions::failures(&self.pool, &format!("register-ip:{ip}")).await?;
            if n >= REGISTRATIONS_PER_IP {
                return Err(ServiceError::RateLimited { retry_after: 900 });
            }
        }
        let email = r.email.trim().to_owned();
        let mut errors = Vec::new();
        if email.is_empty() {
            errors.push(FieldError::new(
                "email",
                "required",
                "Enter your email address.",
            ));
        }
        if let Gate::Invited(Some(bound)) = &gate
            && !bound.eq_ignore_ascii_case(&email)
        {
            errors.push(FieldError::new(
                "email",
                "mismatch",
                "Use the address the invitation was sent to.",
            ));
        }
        if r.password.expose() != r.confirm.expose() {
            errors.push(FieldError::new(
                "confirm_password",
                "mismatch",
                "The passwords don't match.",
            ));
        }
        let checked = match check_account(NewAccount {
            username: r.username.trim().to_owned(),
            email: Some(email.clone()),
            display_name: None,
            password: Some(r.password),
            is_admin: false,
        })
        .await
        {
            Ok(c) if errors.is_empty() => c,
            Ok(_) => return Err(ServiceError::Invalid(errors)),
            Err(ServiceError::Invalid(mut more)) => {
                // The form calls the password field `password` too.
                more.append(&mut errors);
                return Err(ServiceError::Invalid(more));
            }
            Err(e) => return Err(e),
        };
        let mut tx = self.pool.begin().await.map_err(ServiceError::internal)?;
        let id = insert_account(&mut tx, &checked).await?;
        let mut confirmed = false;
        if let Gate::Invited(_) = &gate {
            let t = r.invite.as_deref().unwrap_or_default();
            let Some(bound) =
                slath_db::invitations::use_for(&mut *tx, &token::storage_hash(t), id).await?
            else {
                // Used by someone else in the meantime.
                return Err(ServiceError::NotFound);
            };
            if bound.is_some() {
                slath_db::invitations::confirm_email(&mut *tx, id)
                    .await
                    .map_err(|_| {
                        ServiceError::invalid(
                            "email",
                            "taken",
                            "This email address is already in use.",
                        )
                    })?;
                confirmed = true;
            }
        }
        tx.commit().await.map_err(ServiceError::internal)?;
        if let Some(ip) = ip {
            slath_db::sessions::record_failures(&self.pool, &[format!("register-ip:{ip}")]).await?;
        }
        let who = Subject::User {
            id,
            kind: slath_core::authz::UserKind::Human,
            state: UserState::Active,
            is_site_admin: false,
            credential: slath_core::authz::Credential::Session,
        };
        if confirmed {
            self.audit(
                &who,
                "user.create",
                crate::audit::Target::new("user", Some(*id.as_uuid())),
                serde_json::json!({"username": checked.name.as_str(), "invited": true}),
            )
            .await;
        } else {
            self.after_account_created(&who, id, &checked).await;
        }
        let cookie = self
            .create_session(id, slath_db::sessions::AuthLevel::Password, info)
            .await?;
        Ok((cookie, id))
    }

    /// True if accounts must confirm their address before creating things.
    #[must_use]
    pub fn confirmation_required(&self) -> bool {
        self.config.auth.registration == Registration::Open && self.email_enabled()
    }

    /// Fails unless the subject may create repositories, organizations, and
    /// comments: on open instances with email, a confirmed address is
    /// needed. Site administrators and the system are exempt.
    pub async fn require_confirmed(&self, subject: &Subject) -> Result<()> {
        if !self.confirmation_required() {
            return Ok(());
        }
        match subject {
            // Bots have no email; their owner confirmed theirs.
            Subject::User {
                id,
                kind: slath_core::authz::UserKind::Human,
                is_site_admin: false,
                ..
            } if !slath_db::invitations::email_confirmed(&self.pool, *id).await? => {
                Err(ServiceError::Conflict(
                    "Confirm your email address first: open the link we sent to it.".to_owned(),
                ))
            }
            _ => Ok(()),
        }
    }

    /// Creates an invitation link (site administrators). Returns the link.
    pub async fn create_invitation(
        &self,
        subject: &Subject,
        email: Option<&str>,
        note: Option<&str>,
    ) -> Result<Secret<String>> {
        self.require_site_admin(subject)?;
        let email = email.map(str::trim).filter(|e| !e.is_empty());
        if let Some(e) = email
            && !crate::accounts::valid_email(e)
        {
            return Err(ServiceError::invalid(
                "email",
                "invalid",
                "Enter an email address like name@example.org.",
            ));
        }
        let note = note.map(str::trim).filter(|n| !n.is_empty());
        if note.is_some_and(|n| n.chars().count() > 200 || n.chars().any(char::is_control)) {
            return Err(ServiceError::invalid(
                "note",
                "invalid",
                "Use at most 200 characters.",
            ));
        }
        let t = token::new_session_id().map_err(ServiceError::internal)?;
        let creator = match subject {
            Subject::User { id, .. } => Some(*id),
            _ => None,
        };
        let id = slath_db::invitations::insert(
            &self.pool,
            &slath_db::invitations::NewInvitation {
                token_hash: &token::storage_hash(t.expose()),
                created_by: creator,
                email,
                note,
                expires_at: OffsetDateTime::now_utc() + time::Duration::days(INVITE_DAYS),
            },
        )
        .await?;
        self.audit(
            subject,
            "invitation.create",
            crate::audit::Target::new("invitation", Some(id)),
            serde_json::json!({"bound_to_email": email.is_some()}),
        )
        .await;
        let link = self
            .config
            .server
            .public_url
            .join(&format!("/-/register?invite={}", t.expose()));
        if let Some(to) = email {
            let _ = self
                .queue_email(&crate::email::Email {
                    to: to.to_owned(),
                    subject: "You're invited to Slath".to_owned(),
                    text: format!(
                        "You're invited to create an account at {}. Open this link within {INVITE_DAYS} days:\n\n{link}\n",
                        self.config.server.public_url.origin()
                    ),
                    ..crate::email::Email::default()
                })
                .await;
        }
        Ok(Secret::new(link))
    }

    pub async fn list_invitations(
        &self,
        subject: &Subject,
    ) -> Result<Vec<slath_db::invitations::Invitation>> {
        self.require_site_admin(subject)?;
        Ok(slath_db::invitations::list(&self.pool, 200).await?)
    }

    pub async fn revoke_invitation(&self, subject: &Subject, id: uuid::Uuid) -> Result<()> {
        self.require_site_admin(subject)?;
        if !slath_db::invitations::revoke(&self.pool, id).await? {
            return Err(ServiceError::NotFound);
        }
        self.audit(
            subject,
            "invitation.revoke",
            crate::audit::Target::new("invitation", Some(id)),
            serde_json::json!({}),
        )
        .await;
        Ok(())
    }
}
