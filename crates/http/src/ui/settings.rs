// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Account settings: profile, password, sessions, and sudo confirmation.

use askama::Template;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Redirect, Response};
use axum::{Form, Router};
use serde::Deserialize;
use slath_core::Secret;
use slath_service::accounts::LoginError;
use slath_service::settings::SessionListItem;
use slath_service::{FieldError, ServiceError};

use super::{PageCtx, Time, render};
use crate::AppState;
use crate::auth::{MaybeSession, RequireSession, Sudo, safe_next, session_cookie_name, set_cookie};
use crate::error::AppError;
use crate::routes::{Access, RouteExt};

#[derive(Deserialize, Default)]
pub struct StatusQuery {
    status: Option<String>,
}

/// Success messages after redirects (`?status=`), from a fixed set.
fn status_message(s: Option<&str>) -> Option<&'static str> {
    Some(match s? {
        "profile-saved" => "Profile saved.",
        "password-changed" => "Password changed. Your other sessions were signed out.",
        "session-revoked" => "Session signed out.",
        "key-added" => "SSH key added.",
        "key-removed" => "SSH key removed.",
        "token-revoked" => "Token revoked.",
        "appearance-saved" => "Appearance saved.",
        "mfa-off" => "Two-factor authentication is off.",
        "verification-sent" => "We sent a confirmation link to your email address.",
        "passkey-added" => "Passkey added.",
        "passkey-removed" => "Passkey removed.",
        "passkey-failed" => "The passkey couldn't be added. Try again.",
        "passkey-name" => "Name the passkey (up to 100 characters).",
        "avatar-saved" => "Avatar saved.",
        "oidc-linked" => "Sign-in provider linked.",
        "oidc-unlinked" => "Sign-in provider unlinked.",
        "avatar-removed" => "Avatar removed. Your generated avatar is shown again.",
        _ => return None,
    })
}

fn field_errors(e: ServiceError) -> Result<Vec<FieldError>, AppError> {
    match e {
        ServiceError::Invalid(errors) => Ok(errors),
        other => Err(other.into()),
    }
}

/// The first error message for `field`, for templates.
pub fn error_for<'a>(errors: &'a [FieldError], field: &str) -> Option<&'a str> {
    errors
        .iter()
        .find(|e| e.field == field)
        .map(|e| e.message.as_str())
}

#[derive(Template)]
#[template(path = "settings/profile.html")]
struct ProfilePage<'a> {
    ctx: PageCtx,
    /// Active navigation entry (`profile`, `security`, …).
    tab: &'static str,
    name: &'a str,
    avatar_url: String,
    has_avatar: bool,
    display_name: &'a str,
    email: &'a str,
    email_verified: bool,
    email_enabled: bool,
    message: Option<&'a str>,
    errors: Vec<FieldError>,
}

impl ProfilePage<'_> {
    fn error(&self, field: &str) -> Option<&str> {
        error_for(&self.errors, field)
    }
}

async fn profile(
    State(state): State<AppState>,
    ctx: PageCtx,
    RequireSession(session): RequireSession,
    Query(q): Query<StatusQuery>,
) -> Result<Response, AppError> {
    profile_page(
        &state,
        ctx,
        &session,
        status_message(q.status.as_deref()),
        Vec::new(),
    )
    .await
}

async fn profile_page(
    state: &AppState,
    ctx: PageCtx,
    session: &slath_service::sessions::Session,
    message: Option<&str>,
    errors: Vec<FieldError>,
) -> Result<Response, AppError> {
    let user = slath_db::users::get(&state.pool, session.user_id())
        .await
        .map_err(|e| AppError::internal(&e))?
        .ok_or_else(AppError::not_found)?;
    let (email, verified) = slath_db::email_tokens::email_status(&state.pool, session.user_id())
        .await
        .map_err(|e| AppError::internal(&e))?;
    let avatar_url = state.services.avatar_url(&user.name).await;
    let status = if errors.is_empty() {
        StatusCode::OK
    } else {
        StatusCode::UNPROCESSABLE_ENTITY
    };
    let mut res = render(&ProfilePage {
        ctx,
        tab: "profile",
        name: &user.name,
        has_avatar: avatar_url.ends_with(".png"),
        avatar_url,
        display_name: user.display_name.as_deref().unwrap_or(""),
        email: email.as_deref().unwrap_or(""),
        email_verified: verified,
        email_enabled: state.services.email_enabled(),
        message,
        errors,
    });
    *res.status_mut() = status;
    Ok(res)
}

/// `POST /-/settings/avatar`: upload (multipart field `avatar`) or remove.
async fn avatar_save(
    State(state): State<AppState>,
    ctx: PageCtx,
    RequireSession(session): RequireSession,
    form: axum::extract::Multipart,
) -> Result<Response, AppError> {
    let f = crate::attachments::avatar_form(form).await?;
    let upload = if f.remove { None } else { f.image };
    let removed = upload.is_none();
    if !f.remove && removed {
        let errors = vec![FieldError::new("avatar", "required", "Choose an image.")];
        return profile_page(&state, ctx, &session, None, errors).await;
    }
    match state
        .services
        .set_avatar(
            &session.subject,
            slath_service::avatars::AvatarOwner::Me,
            upload,
        )
        .await
    {
        Ok(()) => {
            let status = if removed {
                "avatar-removed"
            } else {
                "avatar-saved"
            };
            Ok(Redirect::to(&format!("/-/settings/profile?status={status}")).into_response())
        }
        Err(e) => {
            let errors = field_errors(e)?;
            profile_page(&state, ctx, &session, None, errors).await
        }
    }
}

#[derive(Deserialize)]
struct ProfileForm {
    #[serde(default)]
    display_name: String,
    /// Absent: the address stays as it is.
    email: Option<String>,
}

async fn profile_save(
    State(state): State<AppState>,
    ctx: PageCtx,
    RequireSession(session): RequireSession,
    Form(f): Form<ProfileForm>,
) -> Result<Response, AppError> {
    let mut result = state
        .services
        .update_profile(&session, &f.display_name)
        .await;
    if result.is_ok()
        && let Some(email) = &f.email
    {
        result = state.services.change_email(&session, email).await;
    }
    match result {
        Ok(()) => Ok(Redirect::to("/-/settings/profile?status=profile-saved").into_response()),
        Err(e) => {
            let errors = field_errors(e)?;
            let mut res = render(&ProfilePage {
                ctx,
                tab: "profile",
                name: &session.row.user_name,
                avatar_url: state.services.avatar_url(&session.row.user_name).await,
                has_avatar: false,
                display_name: &f.display_name,
                email: f.email.as_deref().unwrap_or(""),
                email_verified: false,
                email_enabled: state.services.email_enabled(),
                message: None,
                errors,
            });
            *res.status_mut() = StatusCode::UNPROCESSABLE_ENTITY;
            Ok(res)
        }
    }
}

/// `POST /-/settings/profile/verify`: sends the confirmation link again.
async fn resend_verification(
    State(state): State<AppState>,
    RequireSession(session): RequireSession,
) -> Result<Response, AppError> {
    state.services.send_verification(session.user_id()).await?;
    Ok(Redirect::to("/-/settings/profile?status=verification-sent").into_response())
}

#[derive(Template)]
#[template(path = "settings/security.html")]
struct SecurityPage<'a> {
    ctx: PageCtx,
    /// Active navigation entry (`profile`, `security`, …).
    tab: &'static str,
    message: Option<&'a str>,
    errors: Vec<FieldError>,
    mfa: slath_service::mfa::MfaStatus,
    admin_needs_mfa: bool,
    passkeys_available: bool,
    passkeys: Vec<PasskeyRow>,
    in_sudo: bool,
    /// Linked OpenID sign-ins.
    identities: Vec<slath_service::oidc::LinkedIdentity>,
    /// Providers that can be linked, as `(key, name)`.
    providers: Vec<(String, String)>,
}

struct PasskeyRow {
    id: String,
    name: String,
    created: Time,
    last_used: Option<Time>,
}

async fn passkey_rows(
    state: &AppState,
    session: &slath_service::sessions::Session,
) -> Result<Vec<PasskeyRow>, AppError> {
    Ok(state
        .services
        .passkey_list(session.user_id())
        .await?
        .into_iter()
        .map(|p| PasskeyRow {
            id: p.id.to_string(),
            name: p.name,
            created: p.created_at.into(),
            last_used: p.last_used_at.map(Into::into),
        })
        .collect())
}

impl SecurityPage<'_> {
    fn error(&self, field: &str) -> Option<&str> {
        error_for(&self.errors, field)
    }
}

async fn security(
    State(state): State<AppState>,
    ctx: PageCtx,
    RequireSession(session): RequireSession,
    Query(q): Query<StatusQuery>,
) -> Result<Response, AppError> {
    Ok(render(&SecurityPage {
        ctx,
        tab: "security",
        message: status_message(q.status.as_deref()),
        errors: Vec::new(),
        mfa: state.services.mfa_status(session.user_id()).await?,
        admin_needs_mfa: state.services.admin_needs_mfa(&session),
        passkeys_available: state.services.passkeys_available(),
        passkeys: passkey_rows(&state, &session).await?,
        in_sudo: session.in_sudo(),
        identities: state.services.oidc_identities(&session).await?,
        providers: state.services.oidc_providers(),
    }))
}

#[derive(Template)]
#[template(path = "settings/totp_setup.html")]
struct TotpSetupPage {
    ctx: PageCtx,
    tab: &'static str,
    message: Option<&'static str>,
    errors: Vec<FieldError>,
    setup: slath_service::mfa::TotpSetup,
}

impl TotpSetupPage {
    fn error(&self, field: &str) -> Option<&str> {
        error_for(&self.errors, field)
    }
}

/// A page with secrets on it: never cached.
fn no_store(mut res: Response) -> Response {
    res.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    res
}

/// `POST /-/settings/security/totp`: starts (or shows the pending) setup.
async fn totp_begin(
    State(state): State<AppState>,
    ctx: PageCtx,
    Sudo(session): Sudo,
) -> Result<Response, AppError> {
    let setup = match state.services.totp_pending(&session).await? {
        Some(s) => s,
        None => state.services.totp_begin(&session).await?,
    };
    Ok(no_store(render(&TotpSetupPage {
        ctx,
        tab: "security",
        message: None,
        errors: Vec::new(),
        setup,
    })))
}

#[derive(Deserialize)]
struct CodeForm {
    #[serde(default)]
    code: String,
}

#[derive(Template)]
#[template(path = "settings/recovery_codes.html")]
struct RecoveryCodesPage {
    ctx: PageCtx,
    tab: &'static str,
    message: Option<&'static str>,
    codes: Vec<String>,
    /// Just turned on (as opposed to regenerated).
    first: bool,
}

/// `POST /-/settings/security/totp/confirm`.
async fn totp_confirm(
    State(state): State<AppState>,
    ctx: PageCtx,
    Sudo(session): Sudo,
    Form(f): Form<CodeForm>,
) -> Result<Response, AppError> {
    match state.services.totp_confirm(&session, &f.code).await {
        Ok(codes) => Ok(no_store(render(&RecoveryCodesPage {
            ctx,
            tab: "security",
            message: None,
            codes: codes.into_iter().map(Secret::into_inner).collect(),
            first: true,
        }))),
        Err(ServiceError::Invalid(errors)) => {
            let setup = state
                .services
                .totp_pending(&session)
                .await?
                .ok_or_else(AppError::not_found)?;
            let mut res = no_store(render(&TotpSetupPage {
                ctx,
                tab: "security",
                message: None,
                errors,
                setup,
            }));
            *res.status_mut() = StatusCode::UNPROCESSABLE_ENTITY;
            Ok(res)
        }
        Err(e) => Err(e.into()),
    }
}

/// `POST /-/settings/security/totp/disable`.
async fn totp_disable(
    State(state): State<AppState>,
    Sudo(session): Sudo,
) -> Result<Response, AppError> {
    state.services.totp_disable(&session).await?;
    Ok(Redirect::to("/-/settings/security?status=mfa-off").into_response())
}

/// `POST /-/settings/security/recovery-codes`.
async fn recovery_codes(
    State(state): State<AppState>,
    ctx: PageCtx,
    Sudo(session): Sudo,
) -> Result<Response, AppError> {
    let codes = state.services.regenerate_recovery_codes(&session).await?;
    Ok(no_store(render(&RecoveryCodesPage {
        ctx,
        tab: "security",
        message: None,
        codes: codes.into_iter().map(Secret::into_inner).collect(),
        first: false,
    })))
}

#[derive(Deserialize)]
struct PasswordForm {
    #[serde(default)]
    new_password: String,
    #[serde(default)]
    confirm_password: String,
}

async fn change_password(
    State(state): State<AppState>,
    ctx: PageCtx,
    Sudo(session): Sudo,
    Form(f): Form<PasswordForm>,
) -> Result<Response, AppError> {
    match state
        .services
        .change_password(
            &session,
            Secret::new(f.new_password),
            Secret::new(f.confirm_password),
        )
        .await
    {
        Ok(_) => Ok(Redirect::to("/-/settings/security?status=password-changed").into_response()),
        Err(e) => {
            let errors = field_errors(e)?;
            let mut res = render(&SecurityPage {
                ctx,
                tab: "security",
                message: None,
                errors,
                mfa: state.services.mfa_status(session.user_id()).await?,
                admin_needs_mfa: state.services.admin_needs_mfa(&session),
                passkeys_available: state.services.passkeys_available(),
                passkeys: passkey_rows(&state, &session).await?,
                in_sudo: session.in_sudo(),
                identities: state.services.oidc_identities(&session).await?,
                providers: state.services.oidc_providers(),
            });
            *res.status_mut() = StatusCode::UNPROCESSABLE_ENTITY;
            Ok(res)
        }
    }
}

/// A session row for the template.
struct SessionView {
    id: String,
    current: bool,
    created: Time,
    last_seen: Time,
    ip: String,
    agent: String,
}

impl From<SessionListItem> for SessionView {
    fn from(s: SessionListItem) -> Self {
        Self {
            id: s.id,
            current: s.current,
            created: s.created_at.into(),
            last_seen: s.last_seen_at.into(),
            ip: s.ip.unwrap_or_else(|| "unknown".to_owned()),
            agent: s.user_agent.map_or_else(
                || "unknown browser".to_owned(),
                |a| a.chars().take(120).collect(),
            ),
        }
    }
}

#[derive(Template)]
#[template(path = "settings/sessions.html")]
struct SessionsPage<'a> {
    ctx: PageCtx,
    /// Active navigation entry (`profile`, `security`, …).
    tab: &'static str,
    sessions: Vec<SessionView>,
    message: Option<&'a str>,
}

async fn sessions(
    State(state): State<AppState>,
    ctx: PageCtx,
    RequireSession(session): RequireSession,
    Query(q): Query<StatusQuery>,
) -> Result<Response, AppError> {
    let list = state.services.list_sessions(&session).await?;
    Ok(render(&SessionsPage {
        ctx,
        tab: "sessions",
        sessions: list.into_iter().map(Into::into).collect(),
        message: status_message(q.status.as_deref()),
    }))
}

async fn revoke_session(
    State(state): State<AppState>,
    RequireSession(session): RequireSession,
    Path(id): Path<String>,
) -> Result<Response, AppError> {
    let own = state
        .services
        .list_sessions(&session)
        .await?
        .into_iter()
        .any(|s| s.id == id && s.current);
    state.services.revoke_session(&session, &id).await?;
    if own {
        let mut res = Redirect::to("/").into_response();
        res.headers_mut().append(
            header::SET_COOKIE,
            set_cookie(&state, session_cookie_name(&state), "", None),
        );
        return Ok(res);
    }
    Ok(Redirect::to("/-/settings/sessions?status=session-revoked").into_response())
}

async fn revoke_all(
    State(state): State<AppState>,
    RequireSession(session): RequireSession,
) -> Result<Response, AppError> {
    state.services.sign_out_everywhere(&session).await?;
    let mut res = Redirect::to("/-/login").into_response();
    res.headers_mut().append(
        header::SET_COOKIE,
        set_cookie(&state, session_cookie_name(&state), "", None),
    );
    Ok(res)
}

#[derive(Template)]
#[template(path = "sudo.html")]
struct SudoPage<'a> {
    ctx: PageCtx,
    next: &'a str,
    error: Option<&'a str>,
    /// The account has a second factor; ask for a code too.
    needs_code: bool,
    /// The account has passkeys (an alternative to password and code).
    has_passkeys: bool,
}

#[derive(Deserialize)]
struct NextQuery {
    next: Option<String>,
    status: Option<String>,
}

async fn sudo_page(
    State(state): State<AppState>,
    ctx: PageCtx,
    RequireSession(session): RequireSession,
    Query(q): Query<NextQuery>,
) -> Result<Response, AppError> {
    let next = safe_next(q.next.as_deref());
    let needs_code = state.services.has_second_factor(session.user_id()).await?;
    let has_passkeys = state.services.passkeys_available()
        && !state
            .services
            .passkey_list(session.user_id())
            .await?
            .is_empty();
    Ok(render(&SudoPage {
        ctx,
        next: &next,
        error: (q.status.as_deref() == Some("passkey-failed"))
            .then_some("The passkey couldn't be verified. Try again."),
        needs_code,
        has_passkeys,
    }))
}

#[derive(Deserialize)]
struct SudoForm {
    #[serde(default)]
    password: String,
    #[serde(default)]
    code: String,
    next: Option<String>,
}

async fn sudo_submit(
    State(state): State<AppState>,
    ctx: PageCtx,
    RequireSession(session): RequireSession,
    Form(f): Form<SudoForm>,
) -> Result<Response, AppError> {
    let next = safe_next(f.next.as_deref());
    let (status, msg) = match state
        .services
        .reauthenticate(&session, Secret::new(f.password), Some(&f.code))
        .await
    {
        Ok(()) => return Ok(Redirect::to(&next).into_response()),
        Err(LoginError::Invalid | LoginError::Expired) => {
            (StatusCode::UNAUTHORIZED, "Incorrect password.")
        }
        Err(LoginError::CodeRequired) => (
            StatusCode::UNAUTHORIZED,
            "Enter the current code from your authenticator app, or a recovery code.",
        ),
        Err(LoginError::Throttled) => (
            StatusCode::TOO_MANY_REQUESTS,
            "Too many failed attempts. Wait 15 minutes and try again.",
        ),
        Err(LoginError::Internal(e)) => return Err(AppError::internal(&e)),
    };
    let needs_code = state.services.has_second_factor(session.user_id()).await?;
    let has_passkeys = state.services.passkeys_available()
        && !state
            .services
            .passkey_list(session.user_id())
            .await?
            .is_empty();
    let mut res = render(&SudoPage {
        ctx,
        next: &next,
        error: Some(msg),
        needs_code,
        has_passkeys,
    });
    *res.status_mut() = status;
    Ok(res)
}

/// `GET /-/avatars/{name}.svg`: a generated identicon. It depends only on the
/// name, so it reveals nothing about whether the account exists.
async fn avatar(
    State(state): State<AppState>,
    Path(file): Path<String>,
) -> Result<Response, AppError> {
    if let Some(sha) = file.strip_suffix(".png") {
        return crate::attachments::avatar_png(&state, sha).await;
    }
    let name = file.strip_suffix(".svg").ok_or_else(AppError::not_found)?;
    if name.is_empty() || name.len() > 64 {
        return Err(AppError::not_found());
    }
    let svg = super::identicon::svg(&name.to_ascii_lowercase());
    Ok((
        [
            (
                header::CONTENT_TYPE,
                HeaderValue::from_static("image/svg+xml"),
            ),
            (
                header::CACHE_CONTROL,
                HeaderValue::from_static("public, max-age=86400"),
            ),
            (
                header::CONTENT_SECURITY_POLICY,
                HeaderValue::from_static("default-src 'none'; style-src 'unsafe-inline'; sandbox"),
            ),
        ],
        svg,
    )
        .into_response())
}

/// A token row for the template.
struct TokenView {
    id: String,
    name: String,
    prefix: String,
    scopes: String,
    created: Time,
    last_used: Option<Time>,
    expires: Option<Time>,
    state: &'static str,
}

#[derive(Template)]
#[template(path = "settings/tokens.html")]
struct TokensPage<'a> {
    ctx: PageCtx,
    tab: &'static str,
    tokens: Vec<TokenView>,
    message: Option<&'a str>,
    errors: Vec<FieldError>,
    scopes: Vec<(&'static str, &'static str, bool)>,
    expiry: Vec<(String, String, bool)>,
    name: String,
    /// A token that was just created, shown once.
    created: Option<String>,
}

impl TokensPage<'_> {
    fn error(&self, field: &str) -> Option<&str> {
        error_for(&self.errors, field)
    }
}

fn scope_choices(is_admin: bool, chosen: &[String]) -> Vec<(&'static str, &'static str, bool)> {
    slath_core::authz::Scope::ALL
        .iter()
        .filter(|s| is_admin || **s != slath_core::authz::Scope::Admin)
        .map(|s| {
            (
                s.as_str(),
                s.description(),
                chosen.iter().any(|c| c == s.as_str()),
            )
        })
        .collect()
}

fn expiry_choices(chosen: Option<u32>) -> Vec<(String, String, bool)> {
    slath_service::tokens::EXPIRY_CHOICES
        .iter()
        .map(|c| match c {
            Some(d) => (d.to_string(), format!("{d} days"), *c == chosen),
            None => ("never".to_owned(), "No expiry".to_owned(), chosen.is_none()),
        })
        .collect()
}

async fn tokens_view(
    state: &AppState,
    session: &slath_service::sessions::Session,
) -> Result<Vec<TokenView>, AppError> {
    let list = state.services.list_tokens(&session.subject).await?;
    Ok(list
        .into_iter()
        .map(|t| TokenView {
            id: t.id.to_string(),
            state: if t.revoked {
                "Revoked"
            } else if t.expired {
                "Expired"
            } else {
                "Active"
            },
            name: t.name,
            prefix: t.prefix,
            scopes: t
                .scopes
                .iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
                .join(", "),
            created: t.created_at.into(),
            last_used: t.last_used_at.map(Into::into),
            expires: t.expires_at.map(Into::into),
        })
        .collect())
}

async fn tokens_page(
    State(state): State<AppState>,
    ctx: PageCtx,
    RequireSession(session): RequireSession,
    Query(q): Query<StatusQuery>,
) -> Result<Response, AppError> {
    let tokens = tokens_view(&state, &session).await?;
    Ok(render(&TokensPage {
        ctx,
        tab: "tokens",
        tokens,
        message: status_message(q.status.as_deref()),
        errors: Vec::new(),
        scopes: scope_choices(session.row.is_admin, &["repo:read".to_owned()]),
        expiry: expiry_choices(Some(slath_service::tokens::DEFAULT_EXPIRY_DAYS)),
        name: String::new(),
        created: None,
    }))
}

async fn create_token(
    State(state): State<AppState>,
    ctx: PageCtx,
    Sudo(session): Sudo,
    body: axum::body::Bytes,
) -> Result<Response, AppError> {
    let pairs = crate::auth::form_pairs(&body);
    let get = |k: &str| pairs.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone());
    let name = get("name").unwrap_or_default();
    let scopes: Vec<String> = pairs
        .iter()
        .filter(|(n, _)| n == "scopes")
        .map(|(_, v)| v.clone())
        .collect();
    let expiry = match get("expires").as_deref() {
        Some("never") => None,
        Some(d) => Some(d.parse::<u32>().unwrap_or(0)),
        None => Some(slath_service::tokens::DEFAULT_EXPIRY_DAYS),
    };
    let result = state
        .services
        .create_token(
            &session,
            slath_service::tokens::NewToken {
                name: name.clone(),
                scopes: scopes.clone(),
                expires_in_days: expiry,
                repo_ids: None,
            },
        )
        .await;
    let (created, errors, status) = match result {
        Ok((secret, _)) => (Some(secret.into_inner()), Vec::new(), StatusCode::OK),
        Err(e) => (None, field_errors(e)?, StatusCode::UNPROCESSABLE_ENTITY),
    };
    let ok = created.is_some();
    let tokens = tokens_view(&state, &session).await?;
    let mut res = render(&TokensPage {
        ctx,
        tab: "tokens",
        tokens,
        message: None,
        errors,
        scopes: scope_choices(session.row.is_admin, if ok { &[] } else { &scopes }),
        expiry: expiry_choices(if ok {
            Some(slath_service::tokens::DEFAULT_EXPIRY_DAYS)
        } else {
            expiry
        }),
        name: if ok { String::new() } else { name },
        created,
    });
    *res.status_mut() = status;
    // The token appears in this response only: keep it out of every cache.
    res.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok(res)
}

async fn revoke_token(
    State(state): State<AppState>,
    RequireSession(session): RequireSession,
    Path(id): Path<String>,
) -> Result<Response, AppError> {
    let id = id.parse().map_err(|_| AppError::not_found())?;
    state.services.revoke_token(&session.subject, id).await?;
    Ok(Redirect::to("/-/settings/tokens?status=token-revoked").into_response())
}

struct KeyView {
    id: String,
    title: String,
    key_type: String,
    fingerprint: String,
    created: Time,
    last_used: Option<Time>,
}

#[derive(Template)]
#[template(path = "settings/keys.html")]
struct KeysPage<'a> {
    ctx: PageCtx,
    tab: &'static str,
    keys: Vec<KeyView>,
    message: Option<&'a str>,
    errors: Vec<FieldError>,
    title: String,
    key: String,
}

impl KeysPage<'_> {
    fn error(&self, field: &str) -> Option<&str> {
        error_for(&self.errors, field)
    }
}

async fn keys_view(
    state: &AppState,
    session: &slath_service::sessions::Session,
) -> Result<Vec<KeyView>, AppError> {
    Ok(state
        .services
        .list_keys(&session.subject)
        .await?
        .into_iter()
        .map(|k| KeyView {
            id: k.id.to_string(),
            title: k.title,
            key_type: k.key_type,
            fingerprint: k.fingerprint,
            created: k.created_at.into(),
            last_used: k.last_used_at.map(Into::into),
        })
        .collect())
}

async fn keys_page(
    State(state): State<AppState>,
    ctx: PageCtx,
    RequireSession(session): RequireSession,
    Query(q): Query<StatusQuery>,
) -> Result<Response, AppError> {
    let keys = keys_view(&state, &session).await?;
    Ok(render(&KeysPage {
        ctx,
        tab: "keys",
        keys,
        message: status_message(q.status.as_deref()),
        errors: Vec::new(),
        title: String::new(),
        key: String::new(),
    }))
}

#[derive(Deserialize)]
struct KeyForm {
    #[serde(default)]
    title: String,
    #[serde(default)]
    key: String,
}

async fn add_key(
    State(state): State<AppState>,
    ctx: PageCtx,
    Sudo(session): Sudo,
    Form(f): Form<KeyForm>,
) -> Result<Response, AppError> {
    match state
        .services
        .add_key(&session.subject, true, &f.title, &f.key)
        .await
    {
        Ok(_) => Ok(Redirect::to("/-/settings/keys?status=key-added").into_response()),
        Err(e) => {
            let errors = field_errors(e)?;
            let keys = keys_view(&state, &session).await?;
            let mut res = render(&KeysPage {
                ctx,
                tab: "keys",
                keys,
                message: None,
                errors,
                title: f.title,
                key: f.key,
            });
            *res.status_mut() = StatusCode::UNPROCESSABLE_ENTITY;
            Ok(res)
        }
    }
}

async fn remove_key(
    State(state): State<AppState>,
    Sudo(session): Sudo,
    Path(id): Path<String>,
) -> Result<Response, AppError> {
    let id = id.parse().map_err(|_| AppError::not_found())?;
    state
        .services
        .remove_key(&session.subject, true, id)
        .await?;
    Ok(Redirect::to("/-/settings/keys?status=key-removed").into_response())
}

#[derive(Template)]
#[template(path = "settings/appearance.html")]
struct AppearancePage<'a> {
    ctx: PageCtx,
    tab: &'static str,
    theme: &'a str,
    density: &'a str,
    code_font_size: i16,
    sizes: std::ops::RangeInclusive<i16>,
    /// `auto`, `git`, or `jj`, and what the pushes came from.
    tool: &'static str,
    detected: Option<&'static str>,
    message: Option<&'a str>,
    errors: Vec<FieldError>,
}

/// The viewer's tool setting for the form: (`auto`/`git`/`jj`, detected).
async fn tool_setting(
    state: &AppState,
    session: &slath_service::sessions::Session,
) -> Result<(&'static str, Option<&'static str>), AppError> {
    use slath_service::tools::Tool;
    let p = state.services.tool_pref(&session.subject).await?;
    let name = |t: Tool| match t {
        Tool::Git => "Git",
        Tool::Jj => "jj",
    };
    Ok((p.setting.map_or("auto", Tool::as_str), p.detected.map(name)))
}

impl AppearancePage<'_> {
    fn error(&self, field: &str) -> Option<&str> {
        error_for(&self.errors, field)
    }
}

async fn appearance(
    State(state): State<AppState>,
    ctx: PageCtx,
    RequireSession(session): RequireSession,
    Query(q): Query<StatusQuery>,
) -> Result<Response, AppError> {
    let (tool, detected) = tool_setting(&state, &session).await?;
    Ok(render(&AppearancePage {
        ctx,
        tab: "appearance",
        theme: &session.row.theme,
        density: &session.row.density,
        code_font_size: session.row.code_font_size,
        sizes: 12..=20,
        tool,
        detected,
        message: status_message(q.status.as_deref()),
        errors: Vec::new(),
    }))
}

#[derive(Deserialize)]
struct AppearanceForm {
    #[serde(default)]
    theme: String,
    #[serde(default)]
    density: String,
    #[serde(default)]
    code_font_size: Option<String>,
    #[serde(default)]
    tool: String,
}

async fn appearance_save(
    State(state): State<AppState>,
    ctx: PageCtx,
    RequireSession(session): RequireSession,
    Form(f): Form<AppearanceForm>,
) -> Result<Response, AppError> {
    let size = f
        .code_font_size
        .as_deref()
        .and_then(|v| v.parse::<i16>().ok())
        .unwrap_or(0);
    let saved = match state
        .services
        .update_appearance(&session, &f.theme, &f.density, size)
        .await
    {
        Ok(()) => state.services.set_vcs_tool(&session.subject, &f.tool).await,
        e => e,
    };
    match saved {
        Ok(()) => Ok(appearance_cookies(
            &state,
            &f.theme,
            &f.density,
            Redirect::to("/-/settings/appearance?status=appearance-saved").into_response(),
        )),
        Err(e) => {
            let errors = field_errors(e)?;
            let (tool, detected) = tool_setting(&state, &session).await?;
            let mut res = render(&AppearancePage {
                ctx,
                tab: "appearance",
                theme: &session.row.theme,
                density: &session.row.density,
                code_font_size: session.row.code_font_size,
                sizes: 12..=20,
                tool,
                detected,
                message: None,
                errors,
            });
            *res.status_mut() = StatusCode::UNPROCESSABLE_ENTITY;
            Ok(res)
        }
    }
}

/// Keeps the anonymous appearance cookies in step with a choice, so the
/// choice also holds after signing out.
fn appearance_cookies(state: &AppState, theme: &str, density: &str, mut res: Response) -> Response {
    const YEAR: i64 = 365 * 24 * 3600;
    let h = res.headers_mut();
    match theme {
        "light" | "dark" => h.append(
            header::SET_COOKIE,
            set_cookie(state, "slath_theme", theme, Some(YEAR)),
        ),
        _ => h.append(
            header::SET_COOKIE,
            set_cookie(state, "slath_theme", "", None),
        ),
    };
    if matches!(density, "comfortable" | "compact") {
        h.append(
            header::SET_COOKIE,
            set_cookie(state, "slath_density", density, Some(YEAR)),
        );
    }
    res
}

#[derive(Deserialize)]
struct MenuForm {
    #[serde(default)]
    theme: String,
    #[serde(default)]
    density: Option<String>,
    #[serde(default)]
    next: Option<String>,
}

/// The header's appearance menu, for everyone: saves the choice for
/// signed-in users and in cookies for everyone, then returns to the page.
async fn appearance_menu(
    State(state): State<AppState>,
    MaybeSession(session): MaybeSession,
    Form(f): Form<MenuForm>,
) -> Result<Response, AppError> {
    let theme = if matches!(f.theme.as_str(), "light" | "dark") {
        f.theme.as_str()
    } else {
        "system"
    };
    let density = match (f.density.as_deref(), &session) {
        (Some(d @ ("comfortable" | "compact")), _) => d.to_owned(),
        (_, Some(s)) => s.row.density.clone(),
        _ => "comfortable".to_owned(),
    };
    if let Some(s) = &session {
        state
            .services
            .update_appearance(s, theme, &density, s.row.code_font_size)
            .await?;
    }
    let to = safe_next(f.next.as_deref());
    Ok(appearance_cookies(
        &state,
        theme,
        &density,
        Redirect::to(&to).into_response(),
    ))
}

pub fn router() -> Router<AppState> {
    use axum::routing::{get, post};
    Router::new()
        .route_as(
            "/-/settings",
            Access::Account,
            get(|| async { Redirect::to("/-/settings/profile") }),
        )
        .route_as(
            "/-/settings/profile",
            Access::Account,
            get(profile).post(profile_save),
        )
        .route_as("/-/settings/avatar", Access::Account, post(avatar_save))
        .route_as(
            "/-/settings/profile/verify",
            Access::Account,
            post(resend_verification),
        )
        .route_as("/-/settings/security", Access::Account, get(security))
        .route_as(
            "/-/settings/security/password",
            Access::Account,
            post(change_password),
        )
        .route_as(
            "/-/settings/security/totp",
            Access::Account,
            post(totp_begin),
        )
        .route_as(
            "/-/settings/security/totp/confirm",
            Access::Account,
            post(totp_confirm),
        )
        .route_as(
            "/-/settings/security/totp/disable",
            Access::Account,
            post(totp_disable),
        )
        .route_as(
            "/-/settings/security/recovery-codes",
            Access::Account,
            post(recovery_codes),
        )
        .route_as("/-/settings/sessions", Access::Account, get(sessions))
        .route_as(
            "/-/settings/sessions/revoke-all",
            Access::Account,
            post(revoke_all),
        )
        .route_as(
            "/-/settings/sessions/{id}/revoke",
            Access::Account,
            post(revoke_session),
        )
        .route_as(
            "/-/settings/keys",
            Access::Account,
            get(keys_page).post(add_key),
        )
        .route_as(
            "/-/settings/keys/{id}/delete",
            Access::Account,
            post(remove_key),
        )
        .route_as(
            "/-/settings/tokens",
            Access::Account,
            get(tokens_page).post(create_token),
        )
        .route_as(
            "/-/settings/tokens/{id}/revoke",
            Access::Account,
            post(revoke_token),
        )
        .route_as(
            "/-/settings/appearance",
            Access::Account,
            get(appearance).post(appearance_save),
        )
        .route_as("/-/appearance", Access::Public, post(appearance_menu))
        .route_as("/-/sudo", Access::Account, get(sudo_page).post(sudo_submit))
        .route_as("/-/avatars/{file}", Access::Public, get(avatar))
}
