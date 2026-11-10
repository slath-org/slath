// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Personal access tokens: creation (shown once), listing, revocation, and
//! authenticating requests that present one.

use slath_auth::token;
use slath_core::Secret;
use slath_core::authz::{Credential, Scope, Subject, UserState};
use slath_core::ids::{RepoId, TokenId, UserId};
use time::OffsetDateTime;

use crate::Services;
use crate::error::{FieldError, Result, ServiceError};
use crate::sessions::Session;

/// Expiry choices offered when creating a token, in days (`None` = never).
pub const EXPIRY_CHOICES: &[Option<u32>] = &[Some(7), Some(30), Some(90), Some(365), None];
/// The default expiry in the UI.
pub const DEFAULT_EXPIRY_DAYS: u32 = 90;

pub struct NewToken {
    pub name: String,
    pub scopes: Vec<String>,
    pub expires_in_days: Option<u32>,
    /// `None`: every repository the user can access.
    pub repo_ids: Option<Vec<RepoId>>,
}

/// A token as its owner sees it (never the secret).
#[derive(Debug, Clone)]
pub struct TokenInfo {
    pub id: TokenId,
    pub name: String,
    pub prefix: String,
    pub scopes: Vec<Scope>,
    pub repo_ids: Option<Vec<RepoId>>,
    pub expires_at: Option<OffsetDateTime>,
    pub last_used_at: Option<OffsetDateTime>,
    pub created_at: OffsetDateTime,
    pub revoked: bool,
    pub expired: bool,
}

/// A request authenticated by a token.
#[derive(Debug, Clone)]
pub struct TokenAuth {
    pub subject: Subject,
    pub user_id: UserId,
    pub token_id: TokenId,
}

fn parse_scopes(raw: &[String]) -> Vec<Scope> {
    raw.iter().filter_map(|s| s.parse().ok()).collect()
}

impl Services {
    /// Creates a token for the session's user (sudo mode). Returns the token
    /// itself, which is shown once and never stored.
    pub async fn create_token(
        &self,
        session: &Session,
        t: NewToken,
    ) -> Result<(Secret<String>, TokenId)> {
        self.require_sudo(session)?;
        let mut errors = Vec::new();
        let name = t.name.trim();
        if name.is_empty() || name.chars().count() > 100 || name.chars().any(char::is_control) {
            errors.push(FieldError::new(
                "name",
                "invalid",
                "Enter a name of 1 to 100 characters.",
            ));
        }
        let mut scopes: Vec<Scope> = Vec::new();
        for raw in &t.scopes {
            match raw.parse::<Scope>() {
                Ok(s) if !scopes.contains(&s) => scopes.push(s),
                Ok(_) => {}
                Err(_) => errors.push(FieldError::new(
                    "scopes",
                    "unknown",
                    format!("Unknown scope {raw:?}."),
                )),
            }
        }
        if scopes.is_empty() {
            errors.push(FieldError::new(
                "scopes",
                "required",
                "Choose at least one scope.",
            ));
        }
        if scopes.contains(&Scope::Admin) && !session.row.is_admin {
            errors.push(FieldError::new(
                "scopes",
                "admin_only",
                "Only site administrators can create admin tokens.",
            ));
        }
        if !EXPIRY_CHOICES.contains(&t.expires_in_days) {
            errors.push(FieldError::new(
                "expires",
                "invalid",
                "Choose one of the offered expiry periods.",
            ));
        }
        if !errors.is_empty() {
            return Err(ServiceError::Invalid(errors));
        }
        scopes.sort();
        let secret = token::new_pat().map_err(ServiceError::internal)?;
        let id = TokenId::new();
        let scope_strings: Vec<String> = scopes.iter().map(|s| s.as_str().to_owned()).collect();
        slath_db::tokens::insert(
            &self.pool,
            &slath_db::tokens::NewToken {
                id,
                user_id: session.user_id(),
                name,
                prefix: &token::pat_display_prefix(secret.expose()),
                hash: &token::storage_hash(secret.expose()),
                scopes: &scope_strings,
                repo_ids: t.repo_ids.as_deref(),
                expires_at: t
                    .expires_in_days
                    .map(|d| OffsetDateTime::now_utc() + time::Duration::days(i64::from(d))),
            },
        )
        .await?;
        self.audit(&session.subject, "token.create", crate::audit::Target::new("token", Some(*id.as_uuid())), serde_json::json!({"name": name, "scopes": scope_strings, "expires_in_days": t.expires_in_days})).await;
        Ok((secret, id))
    }

    /// The user's tokens. Needs a session, or a token with `user:read`.
    pub async fn list_tokens(&self, subject: &Subject) -> Result<Vec<TokenInfo>> {
        let user = user_with_scope(subject, Scope::UserRead)?;
        let now = OffsetDateTime::now_utc();
        Ok(slath_db::tokens::list_for_user(&self.pool, user)
            .await?
            .into_iter()
            .map(|r| TokenInfo {
                id: r.id,
                scopes: parse_scopes(&r.scopes),
                expired: r.expires_at.is_some_and(|e| e <= now),
                revoked: r.revoked_at.is_some(),
                name: r.name,
                prefix: r.prefix,
                repo_ids: r.repo_ids,
                expires_at: r.expires_at,
                last_used_at: r.last_used_at,
                created_at: r.created_at,
            })
            .collect())
    }

    /// Revokes one of the user's tokens. Other users' tokens are "not found".
    pub async fn revoke_token(&self, subject: &Subject, id: TokenId) -> Result<()> {
        let user = user_with_scope(subject, Scope::UserWrite)?;
        if slath_db::tokens::revoke(&self.pool, user, id).await? {
            self.audit(
                subject,
                "token.revoke",
                crate::audit::Target::new("token", Some(*id.as_uuid())),
                serde_json::json!({}),
            )
            .await;
            Ok(())
        } else {
            Err(ServiceError::NotFound)
        }
    }

    /// Authenticates a bearer token. Unknown, malformed, revoked, and expired
    /// tokens and tokens of inactive users all give `None`. A token whose
    /// prefix or checksum is wrong is rejected before any database lookup.
    pub async fn authenticate_token(&self, raw: &str) -> Result<Option<TokenAuth>> {
        if !token::is_pat(raw) {
            return Ok(None);
        }
        let Some(row) =
            slath_db::tokens::find_active(&self.pool, &token::storage_hash(raw)).await?
        else {
            return Ok(None);
        };
        let Some(user) = slath_db::users::get(&self.pool, row.user_id).await? else {
            return Ok(None);
        };
        if user.state != UserState::Active {
            return Ok(None);
        }
        if row
            .last_used_at
            .is_none_or(|t| OffsetDateTime::now_utc() - t > time::Duration::minutes(1))
        {
            slath_db::tokens::touch(&self.pool, row.id).await?;
        }
        let scopes = parse_scopes(&row.scopes);
        // Site-admin power through a token needs the admin scope (and,
        // with require_mfa_for_admins, an account with a second factor).
        let is_site_admin = user.is_admin
            && scopes.contains(&Scope::Admin)
            && self.admin_mfa_satisfied(user.id, None).await?;
        Ok(Some(TokenAuth {
            subject: Subject::User {
                id: user.id,
                kind: user.kind,
                state: user.state,
                is_site_admin,
                credential: Credential::Token {
                    id: row.id,
                    scopes,
                    repo_ids: row.repo_ids,
                },
            },
            user_id: user.id,
            token_id: row.id,
        }))
    }
}

/// The acting user if the subject may act on their own account with `scope`:
/// sessions always, tokens only with the scope.
/// The user behind `subject` if it may act with `scope`: a session, or a
/// token whose scopes imply it.
pub fn user_with_scope(subject: &Subject, scope: Scope) -> Result<UserId> {
    match subject {
        Subject::User {
            id,
            state: UserState::Active,
            credential,
            ..
        } => match credential {
            Credential::Session => Ok(*id),
            // SSH keys authenticate Git transport only, never account actions.
            Credential::SshKey { .. } => Err(ServiceError::Forbidden),
            Credential::Token { scopes, .. } if scopes.iter().any(|s| s.implies(scope)) => Ok(*id),
            Credential::Token { .. } => Err(ServiceError::Forbidden),
        },
        Subject::User { .. } | Subject::Anonymous => Err(ServiceError::Unauthenticated),
        // A deploy key belongs to a repository, not to an account.
        Subject::DeployKey { .. } => Err(ServiceError::Forbidden),
        Subject::System => Err(ServiceError::NotFound),
    }
}
