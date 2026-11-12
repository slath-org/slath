// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Managing bots: a person's (`/-/settings/bots`), an organization's
//! (`/{org}/-/bots`), and each bot's tokens and SSH keys (`/-/bots/{name}`).

use askama::Template;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Redirect, Response};
use axum::{Form, Router};
use serde::Deserialize;
use slath_core::authz::{Scope, Subject, UserState};
use slath_db::bots::{Bot, BotOwner};
use slath_service::bots::{BOT_SCOPES, NewBotToken, Owner};
use slath_service::{FieldError, ServiceError};

use super::settings::error_for;
use super::{PageCtx, Time, render};
use crate::AppState;
use crate::auth::{RequireSession, Sudo};
use crate::error::AppError;
use crate::routes::{Access, RouteExt};

#[derive(Deserialize, Default)]
struct StatusQuery {
    status: Option<String>,
}

fn message(s: Option<&str>) -> Option<&'static str> {
    Some(match s? {
        "created" => {
            "Bot created. Give it a role on the repositories it works on, then a token or an SSH key."
        }
        "token-revoked" => "Token revoked.",
        "key-added" => "SSH key added.",
        "key-removed" => "SSH key removed.",
        "profile-saved" => "Profile saved.",
        "avatar-saved" => "Avatar saved.",
        "avatar-removed" => "Avatar removed. The generated avatar is shown again.",
        _ => return None,
    })
}

/// "Not found" for everything the subject may not manage.
fn hide(e: ServiceError) -> AppError {
    match e {
        ServiceError::Forbidden => AppError::not_found(),
        other => other.into(),
    }
}

/// A bot row for the lists.
struct BotView {
    name: String,
    display_name: Option<String>,
    active: bool,
    created: Time,
}

impl From<Bot> for BotView {
    fn from(b: Bot) -> Self {
        Self {
            active: b.state == UserState::Active,
            name: b.name,
            display_name: b.display_name,
            created: b.created_at.into(),
        }
    }
}

#[derive(Template)]
#[template(path = "settings/bots.html")]
struct MyBotsPage {
    ctx: PageCtx,
    tab: &'static str,
    bots: Vec<BotView>,
    message: Option<&'static str>,
    errors: Vec<FieldError>,
    action: String,
    username: String,
    display_name: String,
}

#[derive(Template)]
#[template(path = "org_bots.html")]
struct OrgBotsPage {
    ctx: PageCtx,
    org: String,
    bots: Vec<BotView>,
    message: Option<&'static str>,
    errors: Vec<FieldError>,
    action: String,
    username: String,
    display_name: String,
}

impl MyBotsPage {
    fn error(&self, field: &str) -> Option<&str> {
        error_for(&self.errors, field)
    }
}

impl OrgBotsPage {
    fn error(&self, field: &str) -> Option<&str> {
        error_for(&self.errors, field)
    }
}

#[derive(Deserialize, Default)]
struct NewBotForm {
    #[serde(default)]
    username: String,
    #[serde(default)]
    display_name: String,
}

/// Renders the list of `owner`'s bots with the form for a new one.
async fn bots_page(
    state: &AppState,
    ctx: PageCtx,
    subject: &Subject,
    owner: Owner<'_>,
    message: Option<&'static str>,
    errors: Vec<FieldError>,
    form: NewBotForm,
) -> Result<Response, AppError> {
    let bots = state
        .services
        .list_bots(subject, owner)
        .await
        .map_err(hide)?
        .into_iter()
        .map(Into::into)
        .collect();
    let status = if errors.is_empty() {
        StatusCode::OK
    } else {
        StatusCode::UNPROCESSABLE_ENTITY
    };
    let mut res = match owner {
        Owner::Me => render(&MyBotsPage {
            ctx,
            tab: "bots",
            bots,
            message,
            errors,
            action: "/-/settings/bots".to_owned(),
            username: form.username,
            display_name: form.display_name,
        }),
        Owner::Org(org) => {
            let org = state
                .services
                .resolve_namespace(org)
                .await?
                .map_or_else(|| org.to_owned(), |(n, _)| n);
            render(&OrgBotsPage {
                ctx,
                action: format!("/{org}/-/bots"),
                org,
                bots,
                message,
                errors,
                username: form.username,
                display_name: form.display_name,
            })
        }
    };
    *res.status_mut() = status;
    Ok(res)
}

async fn create(
    state: &AppState,
    ctx: PageCtx,
    subject: &Subject,
    owner: Owner<'_>,
    f: NewBotForm,
) -> Result<Response, AppError> {
    match state
        .services
        .create_bot(subject, owner, &f.username, &f.display_name)
        .await
    {
        Ok(bot) => {
            Ok(Redirect::to(&format!("/-/bots/{}?status=created", bot.name)).into_response())
        }
        Err(ServiceError::Invalid(errors)) => {
            bots_page(state, ctx, subject, owner, None, errors, f).await
        }
        Err(e) => Err(hide(e)),
    }
}

async fn my_bots(
    State(state): State<AppState>,
    ctx: PageCtx,
    RequireSession(s): RequireSession,
    Query(q): Query<StatusQuery>,
) -> Result<Response, AppError> {
    let msg = message(q.status.as_deref());
    bots_page(
        &state,
        ctx,
        &s.subject,
        Owner::Me,
        msg,
        Vec::new(),
        NewBotForm::default(),
    )
    .await
}

async fn my_bots_create(
    State(state): State<AppState>,
    ctx: PageCtx,
    RequireSession(s): RequireSession,
    Form(f): Form<NewBotForm>,
) -> Result<Response, AppError> {
    create(&state, ctx, &s.subject, Owner::Me, f).await
}

async fn org_bots(
    State(state): State<AppState>,
    ctx: PageCtx,
    RequireSession(s): RequireSession,
    Path(org): Path<String>,
    Query(q): Query<StatusQuery>,
) -> Result<Response, AppError> {
    let msg = message(q.status.as_deref());
    bots_page(
        &state,
        ctx,
        &s.subject,
        Owner::Org(&org),
        msg,
        Vec::new(),
        NewBotForm::default(),
    )
    .await
}

async fn org_bots_create(
    State(state): State<AppState>,
    ctx: PageCtx,
    RequireSession(s): RequireSession,
    Path(org): Path<String>,
    Form(f): Form<NewBotForm>,
) -> Result<Response, AppError> {
    create(&state, ctx, &s.subject, Owner::Org(&org), f).await
}

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

struct KeyView {
    id: String,
    title: String,
    key_type: String,
    fingerprint: String,
    created: Time,
    last_used: Option<Time>,
}

/// The new-token form as submitted (kept after errors).
#[derive(Default)]
struct TokenForm {
    name: String,
    repos: String,
    scopes: Vec<String>,
    expires: Option<u32>,
}

#[derive(Template)]
#[template(path = "bot.html")]
struct BotPage {
    ctx: PageCtx,
    /// The settings entry for a person's bots.
    tab: &'static str,
    bot: BotView,
    /// The owning organization, for an organization's bot.
    org: Option<String>,
    owner: String,
    avatar_url: String,
    has_avatar: bool,
    /// The display name field (as saved, or as submitted after an error).
    display_value: String,
    tokens: Vec<TokenView>,
    keys: Vec<KeyView>,
    message: Option<&'static str>,
    errors: Vec<FieldError>,
    /// A token that was just created, shown once.
    created: Option<String>,
    token_name: String,
    token_repos: String,
    scopes: Vec<(&'static str, &'static str, bool)>,
    expiry: Vec<(u32, bool)>,
    max_days: u32,
    key_title: String,
    key: String,
}

impl BotPage {
    fn error(&self, field: &str) -> Option<&str> {
        error_for(&self.errors, field)
    }
}

/// What the bot page shows besides the bot itself.
#[derive(Default)]
struct BotExtras {
    message: Option<&'static str>,
    /// A submitted display name that wasn't saved.
    display_name: Option<String>,
    errors: Vec<FieldError>,
    created: Option<String>,
    token: TokenForm,
    key_title: String,
    key: String,
}

async fn bot_page(
    state: &AppState,
    ctx: PageCtx,
    subject: &Subject,
    name: &str,
    x: BotExtras,
) -> Result<Response, AppError> {
    let bot = state
        .services
        .managed_bot(subject, name, Scope::UserRead)
        .await
        .map_err(hide)?;
    let now = time::OffsetDateTime::now_utc();
    let tokens = state
        .services
        .list_bot_tokens(subject, &bot.name)
        .await?
        .into_iter()
        .map(|t| TokenView {
            id: t.id.to_string(),
            state: if t.revoked {
                "Revoked"
            } else if t.expires_at.is_some_and(|e| e <= now) {
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
        .collect();
    let keys = state
        .services
        .list_bot_keys(subject, &bot.name)
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
        .collect();
    let (default_days, max_days) = state.services.bot_token_days();
    let chosen = x.token.expires.unwrap_or(default_days);
    let fresh = x.token.scopes.is_empty() && x.errors.is_empty();
    let scopes = BOT_SCOPES
        .iter()
        .map(|s| {
            let on = if fresh {
                *s == Scope::RepoRead
            } else {
                x.token.scopes.iter().any(|c| c == s.as_str())
            };
            (s.as_str(), s.description(), on)
        })
        .collect();
    let status = if x.errors.is_empty() {
        StatusCode::OK
    } else {
        StatusCode::UNPROCESSABLE_ENTITY
    };
    let created = x.created.is_some();
    let avatar_url = state.services.avatar_url(&bot.name).await;
    let display_value = x
        .display_name
        .clone()
        .unwrap_or_else(|| bot.display_name.clone().unwrap_or_default());
    let mut res = render(&BotPage {
        has_avatar: avatar_url.ends_with(".png"),
        avatar_url,
        display_value,
        ctx,
        tab: "bots",
        org: matches!(bot.owner, BotOwner::Org(_)).then(|| bot.owner_name.clone()),
        owner: bot.owner_name.clone(),
        bot: bot.into(),
        tokens,
        keys,
        message: x.message,
        errors: x.errors,
        created: x.created,
        token_name: x.token.name,
        token_repos: x.token.repos,
        scopes,
        expiry: state
            .services
            .bot_token_expiry_choices()
            .into_iter()
            .map(|d| (d, d == chosen))
            .collect(),
        max_days,
        key_title: x.key_title,
        key: x.key,
    });
    *res.status_mut() = status;
    if created {
        // The token appears in this response only: keep it out of every cache.
        res.headers_mut()
            .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    }
    Ok(res)
}

async fn bot(
    State(state): State<AppState>,
    ctx: PageCtx,
    RequireSession(s): RequireSession,
    Path(name): Path<String>,
    Query(q): Query<StatusQuery>,
) -> Result<Response, AppError> {
    let x = BotExtras {
        message: message(q.status.as_deref()),
        ..BotExtras::default()
    };
    bot_page(&state, ctx, &s.subject, &name, x).await
}

async fn create_token(
    State(state): State<AppState>,
    ctx: PageCtx,
    Sudo(session): Sudo,
    Path(name): Path<String>,
    body: axum::body::Bytes,
) -> Result<Response, AppError> {
    let pairs = crate::auth::form_pairs(&body);
    let get = |k: &str| {
        pairs
            .iter()
            .find(|(n, _)| n == k)
            .map(|(_, v)| v.clone())
            .unwrap_or_default()
    };
    let form = TokenForm {
        name: get("name"),
        repos: get("repos"),
        scopes: pairs
            .iter()
            .filter(|(n, _)| n == "scopes")
            .map(|(_, v)| v.clone())
            .collect(),
        expires: get("expires").parse().ok(),
    };
    let result = state
        .services
        .create_bot_token(
            &session,
            &name,
            &NewBotToken {
                name: form.name.clone(),
                scopes: form.scopes.clone(),
                repos: form
                    .repos
                    .split(|c: char| c == ',' || c.is_whitespace())
                    .filter(|r| !r.is_empty())
                    .map(str::to_owned)
                    .collect(),
                // A missing or unreadable choice is reported, not defaulted.
                expires_in_days: Some(form.expires.unwrap_or(0)),
            },
        )
        .await;
    let x = match result {
        Ok((secret, _)) => BotExtras {
            created: Some(secret.into_inner()),
            ..BotExtras::default()
        },
        Err(ServiceError::Invalid(errors)) => BotExtras {
            errors,
            token: form,
            ..BotExtras::default()
        },
        Err(e) => return Err(hide(e)),
    };
    bot_page(&state, ctx, &session.subject, &name, x).await
}

async fn revoke_token(
    State(state): State<AppState>,
    RequireSession(s): RequireSession,
    Path((name, id)): Path<(String, String)>,
) -> Result<Response, AppError> {
    let id = id.parse().map_err(|_| AppError::not_found())?;
    state
        .services
        .revoke_bot_token(&s.subject, &name, id)
        .await
        .map_err(hide)?;
    Ok(Redirect::to(&format!("/-/bots/{name}?status=token-revoked")).into_response())
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
    Path(name): Path<String>,
    Form(f): Form<KeyForm>,
) -> Result<Response, AppError> {
    match state
        .services
        .add_bot_key(&session.subject, true, &name, &f.title, &f.key)
        .await
    {
        Ok(_) => Ok(Redirect::to(&format!("/-/bots/{name}?status=key-added")).into_response()),
        Err(ServiceError::Invalid(errors)) => {
            let x = BotExtras {
                errors,
                key_title: f.title,
                key: f.key,
                ..BotExtras::default()
            };
            bot_page(&state, ctx, &session.subject, &name, x).await
        }
        Err(e) => Err(hide(e)),
    }
}

async fn remove_key(
    State(state): State<AppState>,
    Sudo(session): Sudo,
    Path((name, id)): Path<(String, String)>,
) -> Result<Response, AppError> {
    let id = id.parse().map_err(|_| AppError::not_found())?;
    state
        .services
        .remove_bot_key(&session.subject, true, &name, id)
        .await
        .map_err(hide)?;
    Ok(Redirect::to(&format!("/-/bots/{name}?status=key-removed")).into_response())
}

#[derive(Deserialize)]
struct ProfileForm {
    #[serde(default)]
    display_name: String,
}

async fn save_profile(
    State(state): State<AppState>,
    ctx: PageCtx,
    RequireSession(s): RequireSession,
    Path(name): Path<String>,
    Form(f): Form<ProfileForm>,
) -> Result<Response, AppError> {
    match state
        .services
        .set_bot_display_name(&s.subject, &name, &f.display_name)
        .await
    {
        Ok(()) => Ok(Redirect::to(&format!("/-/bots/{name}?status=profile-saved")).into_response()),
        Err(ServiceError::Invalid(errors)) => {
            let x = BotExtras {
                errors,
                display_name: Some(f.display_name),
                ..BotExtras::default()
            };
            bot_page(&state, ctx, &s.subject, &name, x).await
        }
        Err(e) => Err(hide(e)),
    }
}

/// Uploads (multipart field `avatar`) or removes a bot's avatar.
async fn save_avatar(
    State(state): State<AppState>,
    ctx: PageCtx,
    RequireSession(s): RequireSession,
    Path(name): Path<String>,
    form: axum::extract::Multipart,
) -> Result<Response, AppError> {
    let bot = state
        .services
        .managed_bot(&s.subject, &name, Scope::UserWrite)
        .await
        .map_err(hide)?;
    let f = crate::attachments::avatar_form(form).await?;
    let upload = if f.remove { None } else { f.image };
    let removed = upload.is_none();
    if !f.remove && removed {
        let x = BotExtras {
            errors: vec![FieldError::new("avatar", "required", "Choose an image.")],
            ..BotExtras::default()
        };
        return bot_page(&state, ctx, &s.subject, &name, x).await;
    }
    match state
        .services
        .set_avatar(
            &s.subject,
            slath_service::avatars::AvatarOwner::Bot(bot.id),
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
            Ok(Redirect::to(&format!("/-/bots/{}?status={status}", bot.name)).into_response())
        }
        Err(ServiceError::Invalid(errors)) => {
            let x = BotExtras {
                errors,
                ..BotExtras::default()
            };
            bot_page(&state, ctx, &s.subject, &name, x).await
        }
        Err(e) => Err(hide(e)),
    }
}

pub fn router() -> Router<AppState> {
    use axum::routing::{get, post};
    Router::new()
        .route_as(
            "/-/settings/bots",
            Access::Account,
            get(my_bots).post(my_bots_create),
        )
        .route_as(
            "/{org}/-/bots",
            Access::Resource,
            get(org_bots).post(org_bots_create),
        )
        .route_as("/-/bots/{name}", Access::Resource, get(bot))
        .route_as(
            "/-/bots/{name}/profile",
            Access::Resource,
            post(save_profile),
        )
        .route_as("/-/bots/{name}/avatar", Access::Resource, post(save_avatar))
        .route_as(
            "/-/bots/{name}/tokens",
            Access::Resource,
            post(create_token),
        )
        .route_as(
            "/-/bots/{name}/tokens/{id}/revoke",
            Access::Resource,
            post(revoke_token),
        )
        .route_as("/-/bots/{name}/keys", Access::Resource, post(add_key))
        .route_as(
            "/-/bots/{name}/keys/{id}/delete",
            Access::Resource,
            post(remove_key),
        )
}
