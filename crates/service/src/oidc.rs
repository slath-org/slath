// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Signing in with OpenID Connect providers.
//!
//! Authorization Code flow with PKCE (S256), a single-use `state` that
//! expires after ten minutes and is bound to the browser that started the
//! flow, and a `nonce` checked in the ID token (see
//! [`slath_auth::oidc::validate`]). An identity is `(issuer, subject)`;
//! the email address never links accounts. An unknown identity creates an
//! account only if the provider allows registration and the person confirms
//! a username. If the provider's verified email belongs to an existing
//! account, the person is told to sign in to that account and link the
//! provider there: linking happens only from a signed-in session in sudo
//! mode, so it always needs an existing factor. Accounts with a second
//! factor still need it after an OIDC sign-in unless the provider is
//! configured to count as one. The provider's `picture` is never fetched.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};
use slath_auth::oidc::{self as proto, Discovery, Jwks};
use slath_auth::token;
use slath_core::Secret;
use slath_core::authz::{Credential, Subject, UserKind, UserState};
use slath_core::config::OidcProvider;
use slath_core::ids::UserId;
use slath_db::sessions::AuthLevel;
use time::OffsetDateTime;

use crate::Services;
use crate::error::{Result, ServiceError};
use crate::mfa::SignIn;
use crate::sessions::{NewSessionInfo, Session};

/// Discovery documents and key sets are kept this long.
const CACHE_TTL: Duration = Duration::from_secs(24 * 3600);
/// A key set is fetched again for an unknown key ID at most this often.
const JWKS_REFRESH: Duration = Duration::from_secs(300);
const STATE_TTL: time::Duration = time::Duration::minutes(10);
const PENDING_TTL: time::Duration = time::Duration::minutes(15);
const HTTP_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_RESPONSE: u64 = 1024 * 1024;

/// Cached provider metadata.
#[derive(Debug, Default)]
pub struct OidcCache {
    entries: Mutex<HashMap<String, Cached>>,
}

#[derive(Debug, Clone)]
struct Cached {
    fetched: Instant,
    keys_fetched: Instant,
    discovery: Discovery,
    keys: Jwks,
}

/// What a callback led to.
pub enum OidcResult {
    SignedIn(SignIn),
    /// The identity was linked to the signed-in account.
    Linked,
    /// A new account may be created: confirm a username (the token goes in
    /// a cookie).
    Register {
        token: Secret<String>,
        username: String,
        provider: String,
    },
    /// No account uses this identity and none may be created here.
    NoAccount {
        provider: String,
        /// The provider's verified email belongs to an existing account
        /// (only said when the provider's emails are trusted).
        email_match: bool,
    },
    /// The account linked to this identity is disabled.
    Disabled,
}

/// A linked identity as settings show it.
#[derive(Debug, Clone)]
pub struct LinkedIdentity {
    pub provider_key: String,
    pub provider: String,
    pub issuer: String,
    pub subject: String,
    pub email: Option<String>,
    pub created_at: OffsetDateTime,
}

fn hash(s: &str) -> String {
    use std::fmt::Write as _;
    Sha256::digest(s.as_bytes())
        .iter()
        .fold(String::new(), |mut o, b| {
            let _ = write!(o, "{b:02x}");
            o
        })
}

fn random_token() -> Result<String> {
    token::new_session_id()
        .map(|s| s.expose().clone())
        .map_err(ServiceError::internal)
}

fn invalid(msg: &str) -> ServiceError {
    ServiceError::invalid("oidc", "invalid", msg)
}

/// HTTP for OIDC: no redirects, a timeout, and a response size cap.
fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .max_redirects(0)
        .timeout_global(Some(HTTP_TIMEOUT))
        .http_status_as_error(false)
        .build()
        .new_agent()
}

fn read_json<T: serde::de::DeserializeOwned>(
    res: std::result::Result<ureq::http::Response<ureq::Body>, ureq::Error>,
) -> std::result::Result<T, String> {
    let mut res = res.map_err(|e| e.to_string())?;
    let status = res.status();
    let body = res
        .body_mut()
        .with_config()
        .limit(MAX_RESPONSE)
        .read_to_string()
        .map_err(|e| e.to_string())?;
    if !status.is_success() {
        return Err(format!("HTTP {status}"));
    }
    serde_json::from_str(&body).map_err(|e| e.to_string())
}

async fn get_json<T: serde::de::DeserializeOwned + Send + 'static>(
    url: String,
) -> std::result::Result<T, String> {
    tokio::task::spawn_blocking(move || {
        read_json(
            agent()
                .get(&url)
                .header("accept", "application/json")
                .call(),
        )
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Whether `url` may be used for a provider configured with `issuer`:
/// https, or http only for a loopback issuer (development and tests).
fn endpoint_ok(issuer: &str, url: &str) -> bool {
    let Ok(u) = url::Url::parse(url) else {
        return false;
    };
    match u.scheme() {
        "https" => true,
        "http" => {
            let local = |h: Option<&str>| matches!(h, Some("127.0.0.1" | "localhost" | "[::1]"));
            url::Url::parse(issuer).is_ok_and(|i| i.scheme() == "http" && local(i.host_str()))
                && local(u.host_str())
        }
        _ => false,
    }
}

impl Services {
    /// The configured providers as `(key, display name)`.
    #[must_use]
    pub fn oidc_providers(&self) -> Vec<(String, String)> {
        self.config
            .auth
            .oidc
            .iter()
            .map(|p| (p.key.clone(), p.display_name.clone()))
            .collect()
    }

    fn oidc_provider(&self, key: &str) -> Result<&OidcProvider> {
        self.config
            .auth
            .oidc
            .iter()
            .find(|p| p.key == key)
            .ok_or(ServiceError::NotFound)
    }

    fn oidc_redirect_uri(&self, key: &str) -> String {
        self.config
            .server
            .public_url
            .join(&format!("/-/login/oidc/{key}/callback"))
    }

    /// Discovery and keys of a provider, from the cache or fetched. The
    /// cache is keyed by the configured provider only, so nothing from a
    /// request can change it.
    async fn oidc_metadata(
        &self,
        p: &OidcProvider,
        refresh_keys: bool,
    ) -> Result<(Discovery, Jwks)> {
        let now = Instant::now();
        let cached = self
            .oidc_cache
            .entries
            .lock()
            .ok()
            .and_then(|m| m.get(&p.key).cloned());
        if let Some(c) = &cached
            && now.duration_since(c.fetched) < CACHE_TTL
            && !(refresh_keys && now.duration_since(c.keys_fetched) >= JWKS_REFRESH)
        {
            return Ok((c.discovery.clone(), c.keys.clone()));
        }
        let unavailable = |e: String| {
            tracing::warn!(provider = %p.key, error = %e, "OpenID provider unavailable");
            ServiceError::Unavailable
        };
        let discovery = match &cached {
            Some(c) if now.duration_since(c.fetched) < CACHE_TTL => c.discovery.clone(),
            _ => {
                let url = format!(
                    "{}/.well-known/openid-configuration",
                    p.issuer.trim_end_matches('/')
                );
                let d: Discovery = get_json(url).await.map_err(unavailable)?;
                if d.issuer != p.issuer {
                    return Err(unavailable(format!(
                        "discovery issuer {:?} doesn't match the configured issuer",
                        d.issuer
                    )));
                }
                for e in [&d.authorization_endpoint, &d.token_endpoint, &d.jwks_uri] {
                    if !endpoint_ok(&p.issuer, e) {
                        return Err(unavailable(format!("endpoint {e:?} isn't https")));
                    }
                }
                d
            }
        };
        let keys: Jwks = get_json(discovery.jwks_uri.clone())
            .await
            .map_err(unavailable)?;
        if let Ok(mut m) = self.oidc_cache.entries.lock() {
            let fetched = cached
                .as_ref()
                .filter(|c| now.duration_since(c.fetched) < CACHE_TTL)
                .map_or(now, |c| c.fetched);
            m.insert(
                p.key.clone(),
                Cached {
                    fetched,
                    keys_fetched: now,
                    discovery: discovery.clone(),
                    keys: keys.clone(),
                },
            );
        }
        Ok((discovery, keys))
    }

    /// Starts a sign-in (or, with `link`, linking for that session) and
    /// returns the provider URL to send the browser to. `browser_seed` is
    /// the browser's pre-login cookie (or session ID) the flow is bound to.
    pub async fn oidc_begin(
        &self,
        key: &str,
        browser_seed: &str,
        link: Option<&Session>,
        next: Option<String>,
    ) -> Result<String> {
        let p = self.oidc_provider(key)?;
        if let Some(s) = link {
            self.require_sudo(s)?;
        }
        let (d, _) = self.oidc_metadata(p, false).await?;
        let state = random_token()?;
        let nonce = random_token()?;
        let verifier = random_token()?;
        slath_db::oidc::insert_state(
            &self.pool,
            &hash(&state),
            &slath_db::oidc::State {
                provider_key: p.key.clone(),
                browser_hash: hash(browser_seed),
                nonce: nonce.clone(),
                pkce_verifier: verifier.clone(),
                purpose: if link.is_some() { "link" } else { "login" }.to_owned(),
                user_id: link.map(Session::user_id),
                next,
            },
            OffsetDateTime::now_utc() + STATE_TTL,
        )
        .await?;
        let mut url = url::Url::parse(&d.authorization_endpoint).map_err(ServiceError::internal)?;
        url.query_pairs_mut()
            .append_pair("response_type", "code")
            .append_pair("client_id", &p.client_id)
            .append_pair("redirect_uri", &self.oidc_redirect_uri(&p.key))
            .append_pair("scope", &p.scopes.join(" "))
            .append_pair("state", &state)
            .append_pair("nonce", &nonce)
            .append_pair("code_challenge", &proto::pkce_challenge(&verifier))
            .append_pair("code_challenge_method", "S256");
        Ok(url.into())
    }

    /// Handles the provider's redirect back. `current` is the signed-in
    /// session, if any (needed for linking).
    pub async fn oidc_finish(
        &self,
        key: &str,
        state: &str,
        code: &str,
        browser_seed: &str,
        current: Option<&Session>,
        info: &NewSessionInfo,
    ) -> Result<(OidcResult, Option<String>)> {
        let p = self.oidc_provider(key)?;
        let refused = || invalid("This sign-in link expired or was already used. Try again.");
        let st = slath_db::oidc::take_state(&self.pool, &hash(state))
            .await?
            .ok_or_else(refused)?;
        if st.provider_key != p.key || st.browser_hash != hash(browser_seed) {
            return Err(refused());
        }
        if st.purpose == "link" && st.user_id != current.map(Session::user_id) {
            return Err(refused());
        }
        let claims = self.oidc_exchange(p, code, &st).await?;
        let next = st.next.clone();

        if st.purpose == "link" {
            let user = st.user_id.ok_or_else(refused)?;
            return match slath_db::oidc::link(
                &self.pool,
                &claims.issuer,
                &claims.subject,
                user,
                &p.key,
                claims.email.as_deref().filter(|_| claims.email_verified),
            )
            .await
            {
                Ok(()) => {
                    let who = current.map_or(Subject::Anonymous, |s| s.subject.clone());
                    self.audit(
                        &who,
                        "oidc.link",
                        crate::audit::Target::new("user", Some(*user.as_uuid())),
                        serde_json::json!({"provider": p.key}),
                    )
                    .await;
                    self.notify_security_change(
                        user,
                        &format!(
                            "{} was linked to your account for signing in.",
                            p.display_name
                        ),
                    )
                    .await;
                    Ok((OidcResult::Linked, next))
                }
                Err(slath_db::DbError::Conflict(_)) => {
                    Err(invalid("This sign-in is already linked to an account."))
                }
                Err(e) => Err(e.into()),
            };
        }

        if let Some(identity) =
            slath_db::oidc::find_identity(&self.pool, &claims.issuer, &claims.subject).await?
        {
            let Some(user) = slath_db::users::get(&self.pool, identity.user_id).await? else {
                return Ok((OidcResult::Disabled, next));
            };
            // Bots never sign in, even if an identity points at one.
            if user.state != UserState::Active || user.kind != UserKind::Human {
                return Ok((OidcResult::Disabled, next));
            }
            slath_db::oidc::touch_identity(&self.pool, &claims.issuer, &claims.subject).await?;
            let sign_in = self.oidc_session(p, user.id, info).await?;
            return Ok((OidcResult::SignedIn(sign_in), next));
        }

        let email_match = p.trust_email
            && claims.email_verified
            && match claims.email.as_deref() {
                Some(e) => slath_db::oidc::users_with_verified_email(&self.pool, e).await? == 1,
                None => false,
            };
        if email_match || !p.allow_registration {
            return Ok((
                OidcResult::NoAccount {
                    provider: p.display_name.clone(),
                    email_match,
                },
                next,
            ));
        }
        let token = random_token()?;
        let username = claims
            .preferred_username
            .as_deref()
            .unwrap_or("")
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
            .take(39)
            .collect::<String>();
        slath_db::oidc::insert_pending(
            &self.pool,
            &hash(&token),
            &slath_db::oidc::Pending {
                provider_key: p.key.clone(),
                browser_hash: hash(browser_seed),
                issuer: claims.issuer,
                subject: claims.subject,
                email: claims.email,
                email_verified: claims.email_verified,
                preferred_username: claims.preferred_username,
                display_name: claims.name,
            },
            OffsetDateTime::now_utc() + PENDING_TTL,
        )
        .await?;
        Ok((
            OidcResult::Register {
                token: Secret::new(token),
                username,
                provider: p.display_name.clone(),
            },
            next,
        ))
    }

    /// Exchanges the code and validates the ID token.
    async fn oidc_exchange(
        &self,
        p: &OidcProvider,
        code: &str,
        st: &slath_db::oidc::State,
    ) -> Result<proto::Claims> {
        let failed = |why: &str| {
            tracing::warn!(provider = %p.key, reason = why, "OpenID sign-in refused");
            invalid("The sign-in couldn't be verified. Try again.")
        };
        let (d, keys) = self.oidc_metadata(p, false).await?;
        let secret = tokio::fs::read_to_string(&p.client_secret_file)
            .await
            .map_err(|e| ServiceError::internal(format!("cannot read client_secret_file: {e}")))?;
        let form = [
            ("grant_type", "authorization_code".to_owned()),
            ("code", code.to_owned()),
            ("redirect_uri", self.oidc_redirect_uri(&p.key)),
            ("code_verifier", st.pkce_verifier.clone()),
            ("client_id", p.client_id.clone()),
        ];
        let basic = {
            use base64::Engine;
            let enc =
                |s: &str| url::form_urlencoded::byte_serialize(s.as_bytes()).collect::<String>();
            base64::engine::general_purpose::STANDARD.encode(format!(
                "{}:{}",
                enc(&p.client_id),
                enc(secret.trim())
            ))
        };
        let token_url = d.token_endpoint.clone();
        #[derive(serde::Deserialize)]
        struct TokenResponse {
            id_token: String,
        }
        let res: TokenResponse = tokio::task::spawn_blocking(move || {
            read_json(
                agent()
                    .post(&token_url)
                    .header("accept", "application/json")
                    .header("authorization", &format!("Basic {basic}"))
                    .send_form(form.iter().map(|(k, v)| (*k, v.as_str()))),
            )
        })
        .await
        .map_err(ServiceError::internal)?
        .map_err(|e| failed(&e))?;
        let expect = proto::Expect {
            issuer: &p.issuer,
            client_id: &p.client_id,
            nonce: &st.nonce,
            now: OffsetDateTime::now_utc().unix_timestamp(),
        };
        match proto::validate(&res.id_token, &keys, &expect) {
            Ok(c) => Ok(c),
            // The provider may have rotated its keys.
            Err(proto::TokenError::UnknownKey) => {
                let (_, fresh) = self.oidc_metadata(p, true).await?;
                proto::validate(&res.id_token, &fresh, &expect).map_err(|e| failed(&e.to_string()))
            }
            Err(e) => Err(failed(&e.to_string())),
        }
    }

    /// Signs a user in after the provider vouched for them, asking for the
    /// second factor unless the provider counts as one.
    async fn oidc_session(
        &self,
        p: &OidcProvider,
        user: UserId,
        info: &NewSessionInfo,
    ) -> Result<SignIn> {
        if !p.counts_as_mfa && self.has_second_factor(user).await? {
            let challenge = self
                .start_challenge(
                    user,
                    "login",
                    "oidc",
                    serde_json::json!({"provider": p.key}),
                )
                .await?;
            return Ok(SignIn::SecondFactor(challenge));
        }
        let level = if p.counts_as_mfa {
            AuthLevel::Mfa
        } else {
            AuthLevel::Oidc
        };
        let cookie = self.create_session(user, level, info).await?;
        if let Some(u) = slath_db::users::get(&self.pool, user).await? {
            let who = Subject::User {
                id: u.id,
                kind: u.kind,
                state: u.state,
                is_site_admin: u.is_admin,
                credential: Credential::Session,
            };
            self.audit(
                &who,
                "login.success",
                crate::audit::Target::new("user", Some(*u.id.as_uuid())),
                serde_json::json!({"first_factor": "oidc", "provider": p.key}),
            )
            .await;
        }
        Ok(SignIn::Session(cookie, user))
    }

    /// The pending first sign-in behind a cookie value, as `(provider
    /// name, proposed username)`.
    pub async fn oidc_pending(&self, token: &str, browser_seed: &str) -> Result<(String, String)> {
        let p = slath_db::oidc::pending(&self.pool, &hash(token))
            .await?
            .filter(|p| p.browser_hash == hash(browser_seed))
            .ok_or(ServiceError::NotFound)?;
        let provider = self.oidc_provider(&p.provider_key)?;
        Ok((
            provider.display_name.clone(),
            p.preferred_username.unwrap_or_default(),
        ))
    }

    /// Creates the account for a pending first sign-in.
    pub async fn oidc_register(
        &self,
        token: &str,
        browser_seed: &str,
        username: &str,
        info: &NewSessionInfo,
    ) -> Result<SignIn> {
        let pending = slath_db::oidc::pending(&self.pool, &hash(token))
            .await?
            .filter(|p| p.browser_hash == hash(browser_seed))
            .ok_or(ServiceError::NotFound)?;
        let p = self.oidc_provider(&pending.provider_key)?;
        if !p.allow_registration {
            return Err(ServiceError::NotFound);
        }
        let trusted_email = pending.email_verified && p.trust_email;
        let checked = crate::accounts::check_account(crate::accounts::NewAccount {
            username: username.trim().to_owned(),
            email: pending.email.clone().filter(|_| pending.email_verified),
            display_name: pending.display_name.clone(),
            password: None,
            is_admin: false,
        })
        .await?;
        let mut tx = self.pool.begin().await.map_err(ServiceError::internal)?;
        let id = crate::accounts::insert_account(&mut tx, &checked).await?;
        slath_db::oidc::link(
            &mut *tx,
            &pending.issuer,
            &pending.subject,
            id,
            &p.key,
            pending.email.as_deref().filter(|_| pending.email_verified),
        )
        .await
        .map_err(|e| match e {
            slath_db::DbError::Conflict(_) => {
                invalid("This sign-in is already linked to an account.")
            }
            other => other.into(),
        })?;
        if trusted_email {
            slath_db::invitations::confirm_email(&mut *tx, id).await?;
        }
        slath_db::oidc::delete_pending(&mut *tx, &hash(token)).await?;
        tx.commit().await.map_err(ServiceError::internal)?;
        if !trusted_email {
            self.after_account_created(&Subject::System, id, &checked)
                .await;
        }
        let cookie = self.create_session(id, AuthLevel::Oidc, info).await?;
        Ok(SignIn::Session(cookie, id))
    }

    /// The identities linked to the session's account.
    pub async fn oidc_identities(&self, session: &Session) -> Result<Vec<LinkedIdentity>> {
        let rows = slath_db::oidc::identities(&self.pool, session.user_id()).await?;
        Ok(rows
            .into_iter()
            .map(|i| LinkedIdentity {
                provider: self
                    .oidc_provider(&i.provider_key)
                    .map_or_else(|_| i.provider_key.clone(), |p| p.display_name.clone()),
                provider_key: i.provider_key,
                issuer: i.issuer,
                subject: i.subject,
                email: i.email,
                created_at: i.created_at,
            })
            .collect())
    }

    /// Deletes expired sign-in states and pending registrations. Run
    /// periodically.
    pub async fn purge_oidc(&self) -> Result<()> {
        Ok(slath_db::oidc::purge_expired(&self.pool).await?)
    }

    /// Unlinks an identity (sudo mode).
    pub async fn oidc_unlink(&self, session: &Session, issuer: &str, subject: &str) -> Result<()> {
        self.require_sudo(session)?;
        if !slath_db::oidc::unlink(&self.pool, session.user_id(), issuer, subject).await? {
            return Err(ServiceError::NotFound);
        }
        self.audit(
            &session.subject,
            "oidc.unlink",
            crate::audit::Target::new("user", Some(*session.user_id().as_uuid())),
            serde_json::json!({"issuer": issuer}),
        )
        .await;
        self.notify_security_change(
            session.user_id(),
            "A sign-in provider was unlinked from your account.",
        )
        .await;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoints_must_be_https_except_on_loopback() {
        assert!(endpoint_ok(
            "https://id.example.org",
            "https://id.example.org/token"
        ));
        assert!(endpoint_ok(
            "https://id.example.org",
            "https://other.example.org/keys"
        ));
        assert!(!endpoint_ok(
            "https://id.example.org",
            "http://id.example.org/token"
        ));
        assert!(endpoint_ok(
            "http://127.0.0.1:9000",
            "http://127.0.0.1:9000/token"
        ));
        assert!(!endpoint_ok(
            "http://127.0.0.1:9000",
            "http://10.0.0.1/token"
        ));
        assert!(!endpoint_ok(
            "http://id.example.org",
            "http://id.example.org/token"
        ));
        assert!(!endpoint_ok("https://id.example.org", "file:///etc/passwd"));
    }
}
