// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! An account's email addresses: one primary address (password resets and
//! notifications), and others that only identify the account, for example
//! in commits. Each is confirmed by an emailed link, or by an
//! administrator when email is off. A confirmed address belongs to one
//! account. Only a confirmed address can become the primary one.

use slath_core::authz::{Subject, UserKind};
use slath_core::ids::UserId;
use slath_db::user_emails::{self as db, Address};

use crate::Services;
use crate::email::Email;
use crate::error::{Result, ServiceError};
use crate::sessions::Session;

fn invalid_address() -> ServiceError {
    ServiceError::invalid(
        "email",
        "invalid",
        "Enter an email address like name@example.org.",
    )
}

fn not_yours() -> ServiceError {
    ServiceError::NotFound
}

/// Bots have no email addresses: their commits show as the bot because a
/// bot pushed them, or by its no-reply address.
fn no_bot_addresses() -> ServiceError {
    ServiceError::invalid(
        "email",
        "bot",
        "Bots have no email addresses. Commits a bot pushes, or that use its no-reply address, show as the bot.",
    )
}

impl Services {
    /// The signed-in person's addresses, the primary one first.
    pub async fn emails(&self, session: &Session) -> Result<Vec<Address>> {
        let mut c = self.pool.acquire().await.map_err(ServiceError::internal)?;
        Ok(db::list(&mut c, session.user_id()).await?)
    }

    /// Adds an address and sends its confirmation link. Without a primary
    /// address, it becomes the primary one.
    pub async fn add_email(&self, session: &Session, email: &str) -> Result<()> {
        let email = email.trim();
        if !crate::accounts::valid_email(email) {
            return Err(invalid_address());
        }
        if matches!(
            session.subject,
            Subject::User {
                kind: UserKind::Bot,
                ..
            }
        ) {
            return Err(no_bot_addresses());
        }
        let user = session.user_id();
        let mut c = self.pool.acquire().await.map_err(ServiceError::internal)?;
        let have = db::list(&mut c, user).await?;
        drop(c);
        if have.iter().any(|a| a.email.eq_ignore_ascii_case(email)) {
            return Err(ServiceError::invalid(
                "email",
                "taken",
                "You've already added this address.",
            ));
        }
        if have.is_empty() {
            return self.change_email(session, email).await;
        }
        let max = self.config.limits.emails_per_account;
        if have.len() >= max {
            return Err(ServiceError::invalid(
                "email",
                "limit",
                format!(
                    "An account can have at most {max} addresses (limits.emails_per_account). Remove one first."
                ),
            ));
        }
        match db::insert(&self.pool, user, email).await {
            Ok(()) => {}
            Err(slath_db::DbError::Conflict(_)) => {
                return Err(ServiceError::invalid(
                    "email",
                    "taken",
                    "You've already added this address.",
                ));
            }
            Err(e) => return Err(e.into()),
        }
        self.audit(
            &session.subject,
            "user.email_add",
            crate::audit::Target::new("user", Some(*user.as_uuid())),
            serde_json::json!({}),
        )
        .await;
        self.send_verification_to(user, email).await?;
        Ok(())
    }

    /// Sends the confirmation link of an unconfirmed address again.
    /// Returns whether it did (email may be off).
    pub async fn resend_email_confirmation(&self, session: &Session, email: &str) -> Result<bool> {
        let a = self.own_address(session.user_id(), email).await?;
        if a.verified {
            return Ok(false);
        }
        self.send_verification_to(session.user_id(), &a.email).await
    }

    /// Removes an address. The primary address can only go when it's the
    /// only one: choose another primary address first.
    pub async fn remove_email(&self, session: &Session, email: &str) -> Result<()> {
        let user = session.user_id();
        let a = self.own_address(user, email).await?;
        if a.primary {
            if db::count(&self.pool, user).await? > 0 {
                return Err(ServiceError::invalid(
                    "addresses",
                    "primary",
                    "Make another address primary before you remove this one.",
                ));
            }
            return self.change_email(session, "").await;
        }
        db::remove(&self.pool, user, &a.email).await?;
        self.audit(
            &session.subject,
            "user.email_remove",
            crate::audit::Target::new("user", Some(*user.as_uuid())),
            serde_json::json!({}),
        )
        .await;
        Ok(())
    }

    /// Makes a confirmed address the primary one, and tells the previous
    /// primary address.
    pub async fn make_email_primary(&self, session: &Session, email: &str) -> Result<()> {
        let user = session.user_id();
        let a = self.own_address(user, email).await?;
        if a.primary {
            return Ok(());
        }
        if !a.verified {
            return Err(ServiceError::invalid(
                "addresses",
                "unconfirmed",
                "Confirm this address before you make it primary.",
            ));
        }
        let old = self.emails(session).await?.into_iter().find(|a| a.primary);
        let mut tx = self.pool.begin().await.map_err(ServiceError::internal)?;
        if !db::make_primary(&mut tx, user, &a.email).await? {
            return Err(not_yours());
        }
        tx.commit().await.map_err(ServiceError::internal)?;
        self.audit(
            &session.subject,
            "user.email_primary",
            crate::audit::Target::new("user", Some(*user.as_uuid())),
            serde_json::json!({}),
        )
        .await;
        if let Some(old) = old {
            let _ = self
                .queue_email(&Email {
                    to: old.email,
                    subject: "Your Slath primary email address was changed".to_owned(),
                    text: format!(
                        "Another address of your account at {} is now its primary address: \
                         password resets and notifications go there. If you didn't do this, \
                         sign in, check your security settings, and contact an administrator.\n",
                        self.config.server.public_url.origin()
                    ),
                    ..Email::default()
                })
                .await;
        }
        Ok(())
    }

    /// Adds a confirmed address to an account (administrators, or the
    /// command line), for instances without email. Without a primary
    /// address, it becomes the primary one. Fails if another account
    /// confirmed it.
    pub async fn admin_add_email(
        &self,
        subject: &Subject,
        username: &str,
        email: &str,
    ) -> Result<()> {
        match subject {
            Subject::System => {}
            s => self.require_site_admin(s)?,
        }
        let email = email.trim();
        if !crate::accounts::valid_email(email) {
            return Err(invalid_address());
        }
        let user = slath_db::users::get_by_name(&self.pool, username)
            .await?
            .ok_or(ServiceError::NotFound)?;
        if user.kind == UserKind::Bot {
            return Err(no_bot_addresses());
        }
        let mut tx = self.pool.begin().await.map_err(ServiceError::internal)?;
        db::lock(&mut *tx, email).await?;
        if db::confirmed_owner(&mut *tx, email)
            .await?
            .is_some_and(|o| o != user.id)
        {
            return Err(ServiceError::invalid(
                "email",
                "taken",
                "Another account has already confirmed this address.",
            ));
        }
        let have = db::list(&mut tx, user.id).await?;
        match have.iter().find(|a| a.email.eq_ignore_ascii_case(email)) {
            Some(a) if a.primary => {
                slath_db::invitations::confirm_email(&mut *tx, user.id).await?;
            }
            Some(_) => {
                db::insert_confirmed(&mut *tx, user.id, email).await?;
            }
            None if have.iter().all(|a| !a.primary) => {
                slath_db::email_tokens::set_email(&mut *tx, user.id, Some(email)).await?;
                slath_db::invitations::confirm_email(&mut *tx, user.id).await?;
            }
            None => {
                if have.len() >= self.config.limits.emails_per_account {
                    return Err(ServiceError::invalid(
                        "email",
                        "limit",
                        "The account has as many addresses as it can have (limits.emails_per_account).",
                    ));
                }
                db::insert_confirmed(&mut *tx, user.id, email).await?;
            }
        }
        tx.commit().await.map_err(ServiceError::internal)?;
        self.audit(
            subject,
            "user.email_add",
            crate::audit::Target::new("user", Some(*user.id.as_uuid())),
            serde_json::json!({"username": user.name, "confirmed": true}),
        )
        .await;
        Ok(())
    }

    async fn own_address(&self, user: UserId, email: &str) -> Result<Address> {
        let mut c = self.pool.acquire().await.map_err(ServiceError::internal)?;
        db::list(&mut c, user)
            .await?
            .into_iter()
            .find(|a| a.email.eq_ignore_ascii_case(email.trim()))
            .ok_or_else(not_yours)
    }
}
