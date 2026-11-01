// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Accounts: creation and password sign-in.

use slath_auth::password;
use slath_core::Secret;
use slath_core::authz::{Subject, UserKind, UserState};
use slath_core::ids::UserId;
use slath_core::names::{NameError, NamespaceName};

use crate::Services;
use crate::error::{FieldError, Result, ServiceError};
use crate::sessions::NewSessionInfo;

/// Input for a new human account.
pub struct NewAccount {
    pub username: String,
    pub email: Option<String>,
    pub display_name: Option<String>,
    pub password: Option<Secret<String>>,
    pub is_admin: bool,
}

pub(crate) fn name_error(e: &NameError) -> FieldError {
    let code = match e {
        NameError::Empty => "required",
        NameError::TooLong => "too_long",
        NameError::InvalidStart => "invalid_start",
        NameError::InvalidChar => "invalid_chars",
        NameError::GitSuffix => "git_suffix",
        NameError::Reserved => "reserved",
    };
    FieldError::new("username", code, e.to_string())
}

pub(crate) fn valid_email(e: &str) -> bool {
    let Some((local, domain)) = e.split_once('@') else {
        return false;
    };
    !local.is_empty()
        && domain.contains('.')
        && !domain.starts_with('.')
        && !domain.ends_with('.')
        && e.len() <= 254
        && !e.chars().any(|c| {
            c.is_whitespace() || c.is_control() || matches!(c, '<' | '>' | ',' | ';' | '"')
        })
}

/// Hashes on a blocking thread: Argon2 takes tens of milliseconds of CPU.
pub(crate) async fn hash_password(p: Secret<String>) -> Result<String> {
    tokio::task::spawn_blocking(move || password::hash(&p))
        .await
        .map_err(ServiceError::internal)?
        .map_err(ServiceError::internal)
}

/// A validated new account, with its password hashed.
pub(crate) struct CheckedAccount {
    pub name: NamespaceName,
    pub email: Option<String>,
    pub display_name: Option<String>,
    pub password_hash: Option<String>,
    pub is_admin: bool,
}

/// Validates a new account (all problems at once) and hashes its password.
pub(crate) async fn check_account(a: NewAccount) -> Result<CheckedAccount> {
    let mut errors = Vec::new();
    let name = match NamespaceName::parse(&a.username) {
        Ok(n) => Some(n),
        Err(e) => {
            errors.push(name_error(&e));
            None
        }
    };
    let email = a.email.as_deref().map(str::trim).filter(|e| !e.is_empty());
    if let Some(e) = email
        && !valid_email(e)
    {
        errors.push(FieldError::new(
            "email",
            "invalid",
            "Enter an email address like name@example.org.",
        ));
    }
    let pw = a.password.map(|p| password::normalize(p.expose()));
    if let Some(p) = &pw
        && let Err(e) = password::check_policy(p, &a.username, email)
    {
        errors.push(FieldError::new("password", "policy", e.to_string()));
    }
    let Some(name) = name.filter(|_| errors.is_empty()) else {
        return Err(ServiceError::Invalid(errors));
    };
    let password_hash = match pw {
        Some(p) => Some(hash_password(p).await?),
        None => None,
    };
    Ok(CheckedAccount {
        name,
        email: email.map(str::to_owned),
        display_name: a.display_name.filter(|d| !d.trim().is_empty()),
        password_hash,
        is_admin: a.is_admin,
    })
}

/// Inserts a checked account in the caller's transaction.
pub(crate) async fn insert_account(
    tx: &mut sqlx::PgConnection,
    a: &CheckedAccount,
) -> Result<UserId> {
    slath_db::users::create(
        tx,
        &slath_db::users::NewUser {
            name: a.name.as_str(),
            email: a.email.as_deref(),
            email_verified: false,
            display_name: a.display_name.as_deref(),
            password_hash: a.password_hash.as_deref(),
            is_admin: a.is_admin,
        },
    )
    .await
    .map_err(|e| match e {
        slath_db::DbError::Conflict("name") => {
            ServiceError::invalid("username", "taken", "This name is already taken.")
        }
        slath_db::DbError::Conflict(_) => {
            ServiceError::invalid("email", "taken", "This email address is already in use.")
        }
        other => other.into(),
    })
}

impl Services {
    /// Creates a human account. Allowed for the system (CLI bootstrap) and
    /// site administrators.
    pub async fn create_account(&self, subject: &Subject, a: NewAccount) -> Result<UserId> {
        match subject {
            Subject::System => {}
            Subject::User {
                is_site_admin: true,
                state: UserState::Active,
                ..
            } => {}
            _ => return Err(ServiceError::NotFound),
        }
        let checked = check_account(a).await?;
        let mut tx = self.pool.begin().await.map_err(ServiceError::internal)?;
        let id = insert_account(&mut tx, &checked).await?;
        tx.commit().await.map_err(ServiceError::internal)?;
        self.after_account_created(subject, id, &checked).await;
        Ok(id)
    }

    /// Verification email and audit record of a new account.
    pub(crate) async fn after_account_created(
        &self,
        subject: &Subject,
        id: UserId,
        a: &CheckedAccount,
    ) {
        if a.email.is_some()
            && let Err(e) = self.send_verification(id).await
        {
            tracing::warn!(error = %e, "sending the verification email failed");
        }
        self.audit(
            subject,
            "user.create",
            crate::audit::Target::new("user", Some(*id.as_uuid())),
            serde_json::json!({"username": a.name.as_str(), "admin": a.is_admin}),
        )
        .await;
    }

    /// True if the instance has no accounts yet (the login page then explains
    /// how to create the first administrator).
    pub async fn is_empty_instance(&self) -> Result<bool> {
        Ok(slath_db::users::count(&self.pool).await? == 0)
    }

    /// Password sign-in. Unknown accounts, wrong passwords, bots, and
    /// suspended or deleted accounts all fail the same way, after the same
    /// amount of work. Repeated failures per account and per IP are throttled.
    pub async fn login(
        &self,
        username: &str,
        password: Secret<String>,
        info: NewSessionInfo,
    ) -> Result<crate::mfa::SignIn, LoginError> {
        let limits = &self.config.limits;
        let account_key = format!(
            "account:{}",
            username
                .trim()
                .to_lowercase()
                .chars()
                .take(64)
                .collect::<String>()
        );
        let ip_key = info.ip.as_deref().map(|ip| format!("ip:{ip}"));
        let over = |n: i32, max: u32| i64::from(n) >= i64::from(max);
        let account_failures = slath_db::sessions::failures(&self.pool, &account_key)
            .await
            .map_err(LoginError::from_db)?;
        let ip_failures = match &ip_key {
            Some(k) => slath_db::sessions::failures(&self.pool, k)
                .await
                .map_err(LoginError::from_db)?,
            None => 0,
        };
        if over(account_failures, limits.login_failures_per_account)
            || over(ip_failures, limits.login_failures_per_ip)
        {
            // Throttled responses are deliberately slow and say nothing about the account.
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            return Err(LoginError::Throttled);
        }
        let pw = password::normalize(password.expose());
        let record = slath_db::users::find_for_login(&self.pool, username.trim())
            .await
            .map_err(LoginError::from_db)?;
        let (ok, record) = match record {
            Some(r) => {
                let phc = r.password_hash.clone();
                let pw2 = Secret::new(pw.expose().clone());
                let ok = tokio::task::spawn_blocking(move || match phc {
                    Some(h) => password::verify(&pw2, &h),
                    None => {
                        password::verify_dummy(&pw2);
                        false
                    }
                })
                .await
                .map_err(|e| LoginError::Internal(e.to_string()))?;
                let usable = r.user.kind == UserKind::Human && r.user.state == UserState::Active;
                (ok && usable, Some(r))
            }
            None => {
                let pw2 = Secret::new(pw.expose().clone());
                tokio::task::spawn_blocking(move || password::verify_dummy(&pw2))
                    .await
                    .map_err(|e| LoginError::Internal(e.to_string()))?;
                (false, None)
            }
        };
        let Some(record) = record.filter(|_| ok) else {
            let mut keys = vec![account_key];
            keys.extend(ip_key);
            let counts = slath_db::sessions::record_failures(&self.pool, &keys)
                .await
                .map_err(LoginError::from_db)?;
            // Aggregated: the first failure of a window and reaching the throttle.
            let n = counts.first().copied().unwrap_or(0);
            if n == 1 || i64::from(n) == i64::from(limits.login_failures_per_account) {
                let name: String = username.trim().to_lowercase().chars().take(64).collect();
                self.audit(
                    &Subject::Anonymous,
                    "login.failure",
                    crate::audit::Target::new("user", None),
                    serde_json::json!({"username": name, "failures": n}),
                )
                .await;
            }
            return Err(LoginError::Invalid);
        };
        slath_db::sessions::clear_failures(&self.pool, &account_key)
            .await
            .map_err(LoginError::from_db)?;
        if let Some(phc) = &record.password_hash
            && password::needs_rehash(phc)
            && let Ok(new) = hash_password(pw).await
        {
            slath_db::users::set_password_hash(&self.pool, record.user.id, &new)
                .await
                .map_err(LoginError::from_db)?;
        }
        if self
            .has_second_factor(record.user.id)
            .await
            .map_err(|e| LoginError::Internal(e.to_string()))?
        {
            let challenge = self
                .start_challenge(record.user.id, "login", "password", serde_json::json!({}))
                .await
                .map_err(|e| LoginError::Internal(e.to_string()))?;
            return Ok(crate::mfa::SignIn::SecondFactor(challenge));
        }
        let cookie = self
            .create_session(
                record.user.id,
                slath_db::sessions::AuthLevel::Password,
                &info,
            )
            .await
            .map_err(|e| LoginError::Internal(e.to_string()))?;
        let who = Subject::User {
            id: record.user.id,
            kind: record.user.kind,
            state: record.user.state,
            is_site_admin: record.user.is_admin,
            credential: slath_core::authz::Credential::Session,
        };
        self.audit(
            &who,
            "login.success",
            crate::audit::Target::new("user", Some(*record.user.id.as_uuid())),
            serde_json::json!({}),
        )
        .await;
        Ok(crate::mfa::SignIn::Session(cookie, record.user.id))
    }
}

/// Why a sign-in failed. The variants deliberately don't distinguish an
/// unknown account from a wrong password.
#[derive(Debug, thiserror::Error)]
pub enum LoginError {
    #[error("incorrect username or password")]
    Invalid,
    #[error("too many attempts")]
    Throttled,
    /// The second-factor step expired or was used up; sign in again.
    #[error("sign in again")]
    Expired,
    /// The account has a second factor: a valid code is needed too.
    #[error("a second-factor code is required")]
    CodeRequired,
    #[error("internal error: {0}")]
    Internal(String),
}

impl LoginError {
    fn from_db(e: slath_db::DbError) -> Self {
        Self::Internal(e.to_string())
    }
}

impl Services {
    /// The acting user's own account (`user:read` for tokens).
    pub async fn current_user(&self, subject: &Subject) -> Result<slath_db::users::User> {
        let id = crate::tokens::user_with_scope(subject, slath_core::authz::Scope::UserRead)?;
        slath_db::users::get(&self.pool, id)
            .await?
            .ok_or(ServiceError::NotFound)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn emails() {
        assert!(valid_email("a@example.org"));
        for bad in [
            "",
            "a",
            "a@b",
            "@x.org",
            "a@.org",
            "a@x.",
            "a b@x.org",
            "a@x.org\n",
            "<a@x.org>",
        ] {
            assert!(!valid_email(bad), "{bad:?}");
        }
    }
}
