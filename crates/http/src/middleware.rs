// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Request middleware: request IDs, access logs and metrics, the per-response
//! CSP nonce and security headers, and error rendering.

use std::time::Instant;

use axum::extract::{MatchedPath, Request, State};
use axum::http::{HeaderMap, HeaderName, HeaderValue, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use base64::Engine;

use crate::AppState;
use crate::error::{AppError, ErrorInfo, Problem};

pub const REQUEST_ID: HeaderName = HeaderName::from_static("x-request-id");

/// The ID of the current request, also returned in `X-Request-Id`.
#[derive(Debug, Clone)]
pub struct RequestId(pub String);

/// The CSP nonce of the current response (128 random bits, base64).
#[derive(Debug, Clone)]
pub struct Nonce(pub String);

impl Nonce {
    fn generate() -> Self {
        let mut b = [0u8; 16];
        // getrandom only fails if the OS has no randomness source, in which
        // case nothing else would work either. Fail closed with an unusable
        // nonce rather than a predictable one.
        if getrandom::fill(&mut b).is_err() {
            return Self(String::new());
        }
        Self(base64::engine::general_purpose::STANDARD.encode(b))
    }
}

/// Assigns a request ID, logs the request, and records metrics.
pub async fn request_context(
    State(state): State<AppState>,
    mut req: Request,
    next: Next,
) -> Response {
    let id = uuid::Uuid::now_v7().simple().to_string();
    req.extensions_mut().insert(RequestId(id.clone()));
    let route = req
        .extensions()
        .get::<MatchedPath>()
        .map_or_else(|| "unmatched".to_owned(), |m| m.as_str().to_owned());
    let method = req.method().clone();
    let start = Instant::now();
    let span = tracing::info_span!("request", request_id = %id, %method, route = %route);
    let mut res = tracing::Instrument::instrument(next.run(req), span.clone()).await;
    let elapsed = start.elapsed();
    if let Ok(v) = HeaderValue::from_str(&id) {
        res.headers_mut().insert(REQUEST_ID, v);
    }
    let status = res.status();
    state
        .metrics
        .observe_http(&route, method.as_str(), status.as_u16(), elapsed);
    // Paths and query strings can contain secrets (tokens in URLs from
    // careless clients), so only the route pattern is logged.
    span.in_scope(|| {
        tracing::info!(
            status = status.as_u16(),
            ms = elapsed.as_millis() as u64,
            "request"
        );
    });
    res
}

fn csp_for_html(state: &AppState, nonce: &str) -> String {
    let ui = &state.config.ui;
    let usercontent = state.config.server.usercontent_url.trim_end_matches('/');
    let mut img = String::from("'self' data:");
    if ui.external_images {
        img.push_str(" https:");
    }
    if !usercontent.is_empty() {
        img.push(' ');
        img.push_str(usercontent);
    }
    format!(
        "default-src 'self'; script-src 'self' 'nonce-{nonce}'; style-src 'self'; \
         font-src 'self'; connect-src 'self'; worker-src 'self'; manifest-src 'self'; \
         media-src 'self'; img-src {img}; frame-ancestors 'none'; base-uri 'none'; \
         form-action 'self'; object-src 'none'; report-uri /-/csp-report; report-to csp"
    )
}

/// Generates the response nonce and sets the security headers. HTML
/// responses get the full CSP; other responses keep a CSP their handler set
/// (raw files) or get a restrictive default.
pub async fn security_headers(
    State(state): State<AppState>,
    mut req: Request,
    next: Next,
) -> Response {
    let nonce = Nonce::generate();
    req.extensions_mut().insert(nonce.clone());
    let mut res = next.run(req).await;
    let is_html = res
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|ct| ct.starts_with("text/html"));
    let h = res.headers_mut();
    let set = |h: &mut HeaderMap, name: HeaderName, value: &str| {
        if let Ok(v) = HeaderValue::from_str(value) {
            h.insert(name, v);
        }
    };
    set(h, header::X_CONTENT_TYPE_OPTIONS, "nosniff");
    set(
        h,
        header::REFERRER_POLICY,
        "strict-origin-when-cross-origin",
    );
    if state.config.server.public_url.is_https() {
        set(h, header::STRICT_TRANSPORT_SECURITY, "max-age=31536000");
    }
    if is_html {
        set(
            h,
            header::CONTENT_SECURITY_POLICY,
            &csp_for_html(&state, &nonce.0),
        );
        set(
            h,
            HeaderName::from_static("content-security-policy-report-only"),
            "require-trusted-types-for 'script'; trusted-types htmx; \
             report-uri /-/csp-report; report-to csp",
        );
        set(
            h,
            HeaderName::from_static("reporting-endpoints"),
            "csp=\"/-/csp-report\"",
        );
        set(
            h,
            HeaderName::from_static("cross-origin-opener-policy"),
            "same-origin",
        );
        set(
            h,
            HeaderName::from_static("permissions-policy"),
            "camera=(), microphone=(), geolocation=()",
        );
        set(h, header::X_FRAME_OPTIONS, "DENY");
    } else if !h.contains_key(header::CONTENT_SECURITY_POLICY) {
        set(
            h,
            header::CONTENT_SECURITY_POLICY,
            "default-src 'none'; frame-ancestors 'none'; sandbox",
        );
    }
    res
}

/// True for paths served by the JSON API.
fn is_api(path: &str) -> bool {
    path == "/api" || path.starts_with("/api/")
}

/// Renders error responses: problem JSON under `/api/`, HTML pages elsewhere.
/// Responses carrying [`ErrorInfo`] are rendered; bare error statuses from
/// axum (rejections, the 404 fallback) get the generic error for the status.
pub async fn render_errors(State(state): State<AppState>, req: Request, next: Next) -> Response {
    let path = req.uri().path().to_owned();
    let request_id = req
        .extensions()
        .get::<RequestId>()
        .map_or_else(String::new, |r| r.0.clone());
    let page = crate::ui::PageCtx::from_request(&state, &req);
    let res = next.run(req).await;
    let status = res.status();
    let info = match res.extensions().get::<ErrorInfo>() {
        Some(info) => info.clone(),
        None if status.is_client_error() || status.is_server_error() => {
            let rendered = res.headers().get(header::CONTENT_TYPE).is_some_and(|ct| {
                ct.to_str().is_ok_and(|s| {
                    s.starts_with("text/html") || s.starts_with("application/problem+json")
                })
            });
            if rendered {
                return res;
            }
            *AppError::from_status(status).0
        }
        None => return res,
    };
    let mut out = if is_api(&path) {
        let body = serde_json::to_vec(&Problem::new(&info, &request_id)).unwrap_or_default();
        (
            info.status,
            [(header::CONTENT_TYPE, "application/problem+json")],
            body,
        )
            .into_response()
    } else {
        crate::ui::error_page(&page, &info, &request_id)
    };
    // Keep headers the handler set deliberately (WWW-Authenticate, Allow, Vary).
    for name in [
        header::WWW_AUTHENTICATE,
        header::ALLOW,
        header::VARY,
        header::LOCATION,
    ] {
        if let Some(v) = res.headers().get(&name) {
            out.headers_mut().insert(name, v.clone());
        }
    }
    if let Some(secs) = info.retry_after {
        out.headers_mut()
            .insert(header::RETRY_AFTER, HeaderValue::from(secs));
    }
    if info.status == StatusCode::UNAUTHORIZED
        && is_api(&path)
        && !out.headers().contains_key(header::WWW_AUTHENTICATE)
    {
        out.headers_mut().insert(
            header::WWW_AUTHENTICATE,
            HeaderValue::from_static("Bearer realm=\"Slath\""),
        );
    }
    out
}
