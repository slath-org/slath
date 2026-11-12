// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! API conventions: rate limits, idempotency keys, and signed pagination
//! cursors.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Instant;

use axum::body::Body;
use axum::extract::{FromRequestParts, Request, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use slath_core::authz::Subject;
use slath_db::idempotency::Claim;

use super::ApiSubject;
use crate::AppState;
use crate::auth::ClientIp;
use crate::error::AppError;

/// Token buckets per principal (user ID, or IP for anonymous requests).
#[derive(Debug, Default)]
pub struct RateLimiter {
    buckets: Mutex<HashMap<String, (f64, Instant)>>,
}

impl RateLimiter {
    /// Takes one token; returns (allowed, remaining, seconds until reset).
    fn take(&self, key: &str, per_hour: u32) -> (bool, u32, u64) {
        let cap = f64::from(per_hour.max(1));
        let rate = cap / 3600.0;
        let now = Instant::now();
        let Ok(mut map) = self.buckets.lock() else {
            return (false, 0, 60);
        };
        if map.len() > 100_000 {
            map.retain(|_, (_, t)| now.duration_since(*t).as_secs() < 3600);
        }
        let (tokens, last) = map.entry(key.to_owned()).or_insert((cap, now));
        *tokens = (*tokens + now.duration_since(*last).as_secs_f64() * rate).min(cap);
        *last = now;
        let allowed = *tokens >= 1.0;
        if allowed {
            *tokens -= 1.0;
        }
        let reset = if allowed {
            ((cap - *tokens) / rate).ceil() as u64
        } else {
            ((1.0 - *tokens) / rate).ceil() as u64
        };
        (allowed, tokens.floor() as u32, reset)
    }
}

/// Authenticates once, applies the rate limit, and handles `Idempotency-Key`.
pub async fn api_context(State(state): State<AppState>, req: Request, next: Next) -> Response {
    let (mut parts, body) = req.into_parts();
    let subject = match ApiSubject::from_request_parts(&mut parts, &state).await {
        Ok(s) => s.0,
        Err(e) => return e.into_response(),
    };
    let ip = ClientIp::from_parts(&state, &parts.extensions, &parts.headers);
    let limits = &state.config.limits;
    let (key, per_hour) = match &subject {
        Subject::User {
            id,
            kind: slath_core::authz::UserKind::Bot,
            ..
        } => (format!("u:{id}"), limits.bot_requests_per_hour),
        Subject::User { id, .. } => (format!("u:{id}"), limits.api_requests_per_hour),
        _ => (
            format!(
                "ip:{}",
                ip.0.map_or_else(|| "unknown".to_owned(), |i| i.to_string())
            ),
            limits.api_anonymous_requests_per_hour,
        ),
    };
    let (allowed, remaining, reset) = state.api_limits.take(&key, per_hour);
    let set_limits = |h: &mut HeaderMap| {
        h.insert("ratelimit-limit", HeaderValue::from(per_hour));
        h.insert("ratelimit-remaining", HeaderValue::from(remaining));
        h.insert("ratelimit-reset", HeaderValue::from(reset));
    };
    if !allowed {
        let mut res = AppError::rate_limited(reset).into_response();
        set_limits(res.headers_mut());
        return res;
    }
    let req = Request::from_parts(parts, body);
    let mut res = match idempotency_key(req.headers()) {
        Ok(None) => next.run(req).await,
        Ok(Some(key)) => with_idempotency(&state, &subject, key, req, next).await,
        Err(e) => e.into_response(),
    };
    set_limits(res.headers_mut());
    res
}

fn idempotency_key(headers: &HeaderMap) -> Result<Option<String>, AppError> {
    let Some(v) = headers.get("idempotency-key") else {
        return Ok(None);
    };
    let k = v.to_str().unwrap_or("");
    if k.is_empty()
        || k.len() > 128
        || !k
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    {
        return Err(AppError::validation(vec![crate::error::FieldError {
            field: "Idempotency-Key".to_owned(),
            code: "invalid".to_owned(),
            message: "Use 1 to 128 characters from A-Z, a-z, 0-9, \"_\", and \"-\".".to_owned(),
        }]));
    }
    Ok(Some(k.to_owned()))
}

async fn with_idempotency(
    state: &AppState,
    subject: &Subject,
    key: String,
    req: Request,
    next: Next,
) -> Response {
    if matches!(*req.method(), Method::GET | Method::HEAD | Method::OPTIONS) {
        return next.run(req).await;
    }
    let Subject::User { id, .. } = subject else {
        return next.run(req).await;
    };
    let principal = *id.as_uuid();
    let (parts, body) = req.into_parts();
    let limit = state.config.limits.http_max_body_bytes.as_usize();
    let Ok(bytes) = axum::body::to_bytes(body, limit).await else {
        return AppError::from_status(StatusCode::PAYLOAD_TOO_LARGE).into_response();
    };
    let path = parts
        .uri
        .path_and_query()
        .map_or("", |p| p.as_str())
        .to_owned();
    let mut h = Sha256::new();
    h.update(parts.method.as_str());
    h.update([0]);
    h.update(&path);
    h.update([0]);
    h.update(&bytes);
    let digest = h.finalize();
    let claim = slath_db::idempotency::claim(
        &state.pool,
        principal,
        &key,
        parts.method.as_str(),
        &path,
        &digest,
    )
    .await;
    match claim {
        Err(e) => return AppError::internal(&e).into_response(),
        Ok(Claim::Mismatch) => {
            return AppError::with_code(
                StatusCode::UNPROCESSABLE_ENTITY,
                "idempotency_key_reused",
                "Idempotency key reused",
            )
            .detail("This Idempotency-Key was used for a different request in the last 24 hours.")
            .into_response();
        }
        Ok(Claim::InProgress) => {
            return AppError::with_code(
                StatusCode::CONFLICT,
                "idempotency_in_progress",
                "Request in progress",
            )
            .detail("A request with this Idempotency-Key is still running.")
            .into_response();
        }
        Ok(Claim::Replay { status, response }) => return replay(status, &response),
        Ok(Claim::Fresh) => {}
    }
    let res = next
        .run(Request::from_parts(parts, Body::from(bytes)))
        .await;
    let status = res.status();
    if status.is_server_error() {
        let _ = slath_db::idempotency::release(&state.pool, principal, &key).await;
        return res;
    }
    let (rparts, rbody) = res.into_parts();
    let Ok(rbytes) = axum::body::to_bytes(rbody, 16 * 1024 * 1024).await else {
        let _ = slath_db::idempotency::release(&state.pool, principal, &key).await;
        return AppError::internal(&"response too large to store").into_response();
    };
    let ct = rparts
        .headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_owned();
    let stored = serde_json::json!({
        "content_type": ct,
        "location": rparts.headers.get(header::LOCATION).and_then(|v| v.to_str().ok()),
        "body": String::from_utf8_lossy(&rbytes),
    });
    if let Err(e) = slath_db::idempotency::complete(
        &state.pool,
        principal,
        &key,
        i32::from(status.as_u16()),
        &stored,
    )
    .await
    {
        tracing::warn!(error = %e, "storing an idempotent response failed");
    }
    Response::from_parts(rparts, Body::from(rbytes))
}

fn replay(status: i32, stored: &serde_json::Value) -> Response {
    let status = u16::try_from(status)
        .ok()
        .and_then(|s| StatusCode::from_u16(s).ok())
        .unwrap_or(StatusCode::OK);
    let body = stored
        .get("body")
        .and_then(|b| b.as_str())
        .unwrap_or("")
        .to_owned();
    let mut res = (status, body).into_response();
    let h = res.headers_mut();
    if let Some(ct) = stored
        .get("content_type")
        .and_then(|c| c.as_str())
        .and_then(|c| HeaderValue::from_str(c).ok())
    {
        h.insert(header::CONTENT_TYPE, ct);
    }
    if let Some(l) = stored
        .get("location")
        .and_then(|c| c.as_str())
        .and_then(|c| HeaderValue::from_str(c).ok())
    {
        h.insert(header::LOCATION, l);
    }
    h.insert("idempotency-replayed", HeaderValue::from_static("true"));
    res
}

/// `?limit=&cursor=` of list endpoints.
#[derive(Debug, Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct PageParams {
    /// Items per page (1 to the instance's maximum, default 30).
    pub limit: Option<usize>,
    /// The `next_cursor` of the previous page.
    pub cursor: Option<String>,
}

impl PageParams {
    /// The page size, clamped to `1..=max`.
    #[must_use]
    pub fn limit(&self, max: usize) -> usize {
        self.limit.unwrap_or(30).clamp(1, max)
    }

    /// The offset encoded in a signed cursor (0 without a cursor).
    pub fn offset(&self, state: &AppState) -> Result<usize, AppError> {
        let Some(c) = &self.cursor else { return Ok(0) };
        let (payload, mac) = c.split_once('.').ok_or_else(bad_cursor)?;
        if !state.services.cursors.verify(payload, mac) {
            return Err(bad_cursor());
        }
        let raw = URL_SAFE_NO_PAD.decode(payload).map_err(|_| bad_cursor())?;
        let v: serde_json::Value = serde_json::from_slice(&raw).map_err(|_| bad_cursor())?;
        v.get("o")
            .and_then(serde_json::Value::as_u64)
            .and_then(|o| usize::try_from(o).ok())
            .ok_or_else(bad_cursor)
    }
}

fn bad_cursor() -> AppError {
    AppError::validation(vec![crate::error::FieldError {
        field: "cursor".to_owned(),
        code: "invalid".to_owned(),
        message: "The cursor is invalid; start from the first page.".to_owned(),
    }])
}

/// A signed cursor for the page starting at `offset`.
#[must_use]
pub fn cursor(state: &AppState, offset: usize) -> String {
    let payload = URL_SAFE_NO_PAD.encode(serde_json::json!({ "o": offset }).to_string());
    let mac = state.services.cursors.token(&payload);
    format!("{payload}.{mac}")
}

/// Cuts one page out of `items` (fetched with one extra to detect more) and
/// returns the page with the next cursor and a `Link` header.
pub fn page<T: serde::Serialize>(
    state: &AppState,
    uri: &axum::http::Uri,
    mut items: Vec<T>,
    offset: usize,
    limit: usize,
) -> Response {
    let more = items.len() > limit;
    items.truncate(limit);
    let next = more.then(|| cursor(state, offset + limit));
    let mut res =
        axum::Json(serde_json::json!({ "items": items, "next_cursor": next })).into_response();
    if let Some(n) = &next {
        let link = format!("<{}?limit={limit}&cursor={n}>; rel=\"next\"", uri.path());
        if let Ok(v) = HeaderValue::from_str(&link) {
            res.headers_mut().insert(header::LINK, v);
        }
    }
    res
}
