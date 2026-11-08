// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Server-rendered pages (Askama). Every page works without JavaScript;
//! htmx requests get fragments of the same templates.

pub mod account;
pub mod admin;
pub mod browse;
pub mod design;
pub mod editor;
pub mod identicon;
pub mod import;
pub mod issues;
pub mod labels;
pub mod merge_requests;
pub mod notifications;
pub mod oidc;
pub mod passkeys;
pub mod profiles;
pub mod recovery;
pub mod registration;
pub mod repos;
pub mod search;
pub mod secret_scan;
pub mod settings;
pub mod teams;

use askama::Template;
use axum::extract::{FromRequestParts, Request};
use axum::http::request::Parts;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{Html, IntoResponse, Response};

use crate::AppState;
use crate::error::{AppError, ErrorInfo};
use crate::middleware::Nonce;
use crate::routes::{Access, RouteExt};

/// The signed-in user as the page header shows them.
#[derive(Debug, Clone)]
pub struct NavUser {
    pub name: String,
    pub is_admin: bool,
    /// The uploaded avatar, or the pattern generated from the name.
    pub avatar_url: String,
}

impl NavUser {
    #[must_use]
    pub fn from_session(row: &slath_db::sessions::SessionRow) -> Self {
        Self {
            name: row.user_name.clone(),
            is_admin: row.is_admin,
            avatar_url: match &row.avatar_sha256 {
                Some(sha) => format!("/-/avatars/{sha}.png"),
                None => format!("/-/avatars/{}.svg", row.user_name),
            },
        }
    }
}

/// The logo in the header and as the favicon, one image per theme. Pages
/// only use these URLs, so a logo set by the instance can replace the
/// bundled one here.
#[derive(Debug, Clone)]
pub struct Logo {
    pub light_url: String,
    pub dark_url: String,
}

impl Logo {
    #[must_use]
    pub fn bundled(assets: &crate::assets::Assets) -> Self {
        Self {
            light_url: assets.url("logo-light.svg"),
            dark_url: assets.url("logo-dark.svg"),
        }
    }
}

/// Per-request data every page template needs.
#[derive(Debug, Clone)]
pub struct PageCtx {
    /// The signed-in user, if any.
    pub user: Option<NavUser>,
    /// CSP nonce of this response; stamped on scripts and htmx elements.
    pub nonce: String,
    /// CSRF token for forms and htmx requests (signed-in users only).
    pub csrf: Option<String>,
    /// Explicit theme (`light`/`dark`); `None` follows the system.
    pub theme: Option<&'static str>,
    /// `comfortable` or `compact`.
    pub density: &'static str,
    /// The signed-in user's code font size in px, if not the default.
    pub code_size: Option<i16>,
    pub app_url: String,
    pub style_url: String,
    /// The regular interface font, preloaded by every page.
    pub font_url: String,
    /// The icon sprite; icons are `<use href="{icons_url}#name">`.
    pub icons_url: String,
    /// The logo for light and dark themes (the bundled Slath logo).
    pub logo: Logo,
    /// Path and query of this request (for "sign in and come back").
    pub here: String,
    /// Read-only maintenance is on (an upgrade is running).
    pub maintenance: bool,
    /// The user is an administrator who must sign in with a second factor
    /// before administration works.
    pub admin_needs_mfa: bool,
    /// The user must confirm their address before creating anything.
    pub needs_confirmation: bool,
    /// The Markdown editor mode: `wysiwyg` or `source`.
    pub editor_mode: &'static str,
    /// User content may load images from other sites.
    pub external_images: bool,
}

/// Returns the value of cookie `name`.
#[must_use]
pub fn cookie<'a>(headers: &'a axum::http::HeaderMap, name: &str) -> Option<&'a str> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|kv| kv.trim().split_once('='))
        .find(|(k, _)| *k == name)
        .map(|(_, v)| v)
}

impl PageCtx {
    fn build(
        state: &AppState,
        extensions: &axum::http::Extensions,
        headers: &axum::http::HeaderMap,
        uri: &axum::http::Uri,
    ) -> Self {
        let nonce = extensions
            .get::<Nonce>()
            .map_or_else(String::new, |n| n.0.clone());
        let theme = match cookie(headers, "slath_theme") {
            Some("light") => Some("light"),
            Some("dark") => Some("dark"),
            _ => None,
        };
        let density = match cookie(headers, "slath_density") {
            Some("compact") => "compact",
            _ => "comfortable",
        };
        let session = extensions
            .get::<crate::auth::MaybeSession>()
            .and_then(|m| m.0.as_ref());
        let csrf = extensions
            .get::<crate::auth::CsrfSeed>()
            .map(|seed| state.services.csrf.token(&seed.value));
        // The user's saved theme wins over the anonymous cookie.
        let theme = match session.map(|s| s.row.theme.as_str()) {
            Some("light") => Some("light"),
            Some("dark") => Some("dark"),
            Some(_) => None,
            None => theme,
        };
        let density = match session.map(|s| s.row.density.as_str()) {
            Some("compact") => "compact",
            Some(_) => "comfortable",
            None => density,
        };
        let code_size = session
            .map(|s| s.row.code_font_size)
            .filter(|n| (12..=20).contains(n) && *n != 14);
        Self {
            user: session.map(|s| NavUser::from_session(&s.row)),
            nonce,
            csrf,
            theme,
            density,
            code_size,
            app_url: state.assets.url("app.js"),
            style_url: state.assets.url("style.css"),
            font_url: state.assets.url("SourceSans3-Upright.woff2"),
            icons_url: state.assets.url("icons.svg"),
            logo: Logo::bundled(&state.assets),
            here: uri
                .path_and_query()
                .map_or_else(|| "/".to_owned(), |p| p.as_str().to_owned()),
            maintenance: state.services.maintenance().is_on(),
            admin_needs_mfa: session.is_some_and(|s| state.services.admin_needs_mfa(s)),
            external_images: state.config.ui.external_images,
            editor_mode: match session.map(|s| s.row.editor_mode.as_str()) {
                Some("source") => "source",
                _ => "wysiwyg",
            },
            needs_confirmation: session.is_some_and(|s| {
                !s.row.email_confirmed && !s.row.is_admin && state.services.confirmation_required()
            }),
        }
    }

    #[must_use]
    pub fn from_request(state: &AppState, req: &Request) -> Self {
        Self::build(state, req.extensions(), req.headers(), req.uri())
    }

    /// The CSRF token for forms (empty if none could be computed).
    #[must_use]
    pub fn csrf_token(&self) -> &str {
        self.csrf.as_deref().unwrap_or("")
    }
}

impl FromRequestParts<AppState> for PageCtx {
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        Ok(Self::build(
            state,
            &parts.extensions,
            &parts.headers,
            &parts.uri,
        ))
    }
}

/// Whether the request came from htmx and wants a fragment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HxRequest {
    pub htmx: bool,
    /// `HX-Request-Type: partial` (as opposed to `full`).
    pub partial: bool,
}

impl<S: Send + Sync> FromRequestParts<S> for HxRequest {
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(parts: &mut Parts, _: &S) -> Result<Self, Self::Rejection> {
        let htmx = parts
            .headers
            .get("hx-request")
            .is_some_and(|v| v.as_bytes() == b"true");
        let partial = htmx
            && parts
                .headers
                .get("hx-request-type")
                .is_none_or(|v| v.as_bytes() == b"partial");
        Ok(Self { htmx, partial })
    }
}

/// HTML produced by `slath-markup` (sanitized Markdown, escaped and
/// highlighted code). Templates output it unescaped; nothing else may
/// construct it.
#[derive(Debug, Clone)]
pub struct MarkupHtml(String);

impl MarkupHtml {
    /// Sanitized Markdown.
    #[must_use]
    pub fn markdown(src: &str, external_images: bool) -> Self {
        Self(slath_markup::render_markdown(
            src,
            slath_markup::MarkdownOptions { external_images },
        ))
    }

    /// Wraps output of `slath_markup` functions that escape their input.
    #[must_use]
    pub fn from_markup(html: String) -> Self {
        Self(html)
    }
}

impl std::fmt::Display for MarkupHtml {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl askama::filters::HtmlSafe for MarkupHtml {}

/// A timestamp for templates: `<time datetime="{iso}">{text}</time>` (UTC).
#[derive(Debug, Clone)]
pub struct Time {
    pub iso: String,
    pub text: String,
}

impl From<time::OffsetDateTime> for Time {
    fn from(t: time::OffsetDateTime) -> Self {
        let t = t.to_offset(time::UtcOffset::UTC);
        let iso = t
            .format(&time::format_description::well_known::Rfc3339)
            .unwrap_or_default();
        let text = format!(
            "{:04}-{:02}-{:02} {:02}:{:02} UTC",
            t.year(),
            u8::from(t.month()),
            t.day(),
            t.hour(),
            t.minute()
        );
        Self { iso, text }
    }
}

/// Renders a template as an HTML response.
pub fn render(t: &impl Template) -> Response {
    match t.render() {
        Ok(html) => Html(html).into_response(),
        Err(e) => AppError::internal(&e).into_response(),
    }
}

/// Renders the fragment for partial htmx requests and the full page
/// otherwise, with `Vary: HX-Request` so caches keep them apart.
pub fn render_hx(hx: HxRequest, full: &impl Template, fragment: &impl Template) -> Response {
    let mut res = if hx.partial {
        render(fragment)
    } else {
        render(full)
    };
    res.headers_mut()
        .insert(header::VARY, HeaderValue::from_static("HX-Request"));
    res
}

#[derive(Template)]
#[template(path = "error.html")]
struct ErrorPage<'a> {
    ctx: &'a PageCtx,
    status: u16,
    title: &'a str,
    detail: Option<&'a str>,
    request_id: &'a str,
    signed_out: bool,
}

/// The HTML error page for `info`.
#[must_use]
pub fn error_page(ctx: &PageCtx, info: &ErrorInfo, request_id: &str) -> Response {
    let page = ErrorPage {
        ctx,
        status: info.status.as_u16(),
        title: info.title,
        detail: info.detail.as_deref(),
        request_id,
        signed_out: info.status == StatusCode::UNAUTHORIZED,
    };
    let mut res = render(&page);
    *res.status_mut() = info.status;
    res
}

#[derive(Template)]
#[template(path = "home.html")]
struct Home {
    ctx: PageCtx,
    repos: Vec<repos::RepoCard>,
}

/// `GET /`: recently active public repositories.
pub async fn home(
    axum::extract::State(state): axum::extract::State<AppState>,
    ctx: PageCtx,
) -> Result<Response, AppError> {
    let repos = slath_db::repos::list_public(&state.pool, 50)
        .await
        .map_err(|e| AppError::internal(&e))?
        .iter()
        .map(Into::into)
        .collect();
    Ok(render(&Home { ctx, repos }))
}

/// The 404 fallback for unknown UI routes.
pub async fn fallback() -> AppError {
    AppError::not_found()
}

/// Routes of the UI.
pub fn router() -> axum::Router<AppState> {
    use axum::routing::get;
    let r = axum::Router::new()
        .route_as("/", Access::Public, get(home))
        .merge(account::router())
        .merge(recovery::router())
        .merge(registration::router())
        .merge(passkeys::router())
        .merge(oidc::router())
        .merge(editor::router())
        .merge(teams::router())
        .merge(issues::router())
        .merge(merge_requests::router())
        .merge(labels::router())
        .merge(secret_scan::router())
        .merge(notifications::router())
        .merge(search::router())
        .merge(import::router())
        .merge(settings::router())
        .merge(profiles::router())
        .merge(repos::router())
        .merge(browse::router())
        .merge(admin::router())
        .merge(crate::attachments::ui_router())
        .merge(crate::references::ui_router())
        .route_as(
            "/-/assets/{hash}/{*file}",
            Access::Public,
            get(crate::assets::serve),
        );
    if cfg!(debug_assertions) {
        r.merge(design::router())
    } else {
        r
    }
}
