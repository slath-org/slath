// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Errors of UI and API handlers.
//!
//! A handler returns [`AppError`]; its response carries an [`ErrorInfo`]
//! extension and no body. The `render_errors` middleware, which knows the
//! request (path, request ID, CSP nonce, theme), turns it into RFC 9457
//! problem JSON for `/api/` and an HTML error page otherwise. Internal
//! errors are logged with the request ID and never shown to clients.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Serialize;

/// One field-level validation problem.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FieldError {
    pub field: String,
    pub code: String,
    /// Human-readable message (shown in the UI's error summary).
    #[serde(skip_serializing_if = "String::is_empty")]
    pub message: String,
}

/// The data an error response is rendered from.
#[derive(Debug, Clone)]
pub struct ErrorInfo {
    pub status: StatusCode,
    /// Stable machine-readable code, e.g. `not_found`.
    pub code: &'static str,
    pub title: &'static str,
    pub detail: Option<String>,
    pub errors: Vec<FieldError>,
    /// Seconds for `Retry-After` (429 and 503).
    pub retry_after: Option<u64>,
}

#[derive(Debug, Clone)]
pub struct AppError(pub Box<ErrorInfo>);

impl AppError {
    fn new(status: StatusCode, code: &'static str, title: &'static str) -> Self {
        Self(Box::new(ErrorInfo {
            status,
            code,
            title,
            detail: None,
            errors: Vec::new(),
            retry_after: None,
        }))
    }

    #[must_use]
    pub fn not_found() -> Self {
        Self::new(StatusCode::NOT_FOUND, "not_found", "Not found")
    }

    #[must_use]
    pub fn unauthenticated() -> Self {
        Self::new(
            StatusCode::UNAUTHORIZED,
            "unauthenticated",
            "Authentication required",
        )
    }

    #[must_use]
    pub fn forbidden() -> Self {
        Self::new(StatusCode::FORBIDDEN, "forbidden", "Forbidden")
    }

    #[must_use]
    pub fn conflict(detail: impl Into<String>) -> Self {
        Self::new(StatusCode::CONFLICT, "conflict", "Conflict").detail(detail)
    }

    #[must_use]
    pub fn bad_request(detail: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, "bad_request", "Bad request").detail(detail)
    }

    #[must_use]
    pub fn validation(errors: Vec<FieldError>) -> Self {
        let mut e = Self::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "validation_failed",
            "Validation failed",
        );
        e.0.errors = errors;
        e
    }

    #[must_use]
    pub fn rate_limited(retry_after: u64) -> Self {
        let mut e = Self::new(
            StatusCode::TOO_MANY_REQUESTS,
            "rate_limited",
            "Too many requests",
        );
        e.0.retry_after = Some(retry_after);
        e
    }

    #[must_use]
    pub fn unavailable() -> Self {
        Self::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "unavailable",
            "Service unavailable",
        )
    }

    /// Logs `cause` (never shown to the client) and returns a 500.
    #[must_use]
    pub fn internal(cause: &dyn std::fmt::Display) -> Self {
        tracing::error!(error = %cause, "internal error");
        Self::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal",
            "Internal error",
        )
    }

    /// A specific code for a status, e.g. `(409, "idempotency_in_progress")`.
    #[must_use]
    pub fn with_code(status: StatusCode, code: &'static str, title: &'static str) -> Self {
        Self::new(status, code, title)
    }

    #[must_use]
    pub fn detail(mut self, detail: impl Into<String>) -> Self {
        self.0.detail = Some(detail.into());
        self
    }

    #[must_use]
    pub fn status(&self) -> StatusCode {
        self.0.status
    }

    #[must_use]
    pub fn code(&self) -> &'static str {
        self.0.code
    }

    /// The generic error for a bare status code (axum rejections, 404 fallback).
    #[must_use]
    pub fn from_status(status: StatusCode) -> Self {
        match status {
            StatusCode::NOT_FOUND => Self::not_found(),
            StatusCode::UNAUTHORIZED => Self::unauthenticated(),
            StatusCode::FORBIDDEN => Self::forbidden(),
            StatusCode::METHOD_NOT_ALLOWED => {
                Self::new(status, "method_not_allowed", "Method not allowed")
            }
            StatusCode::PAYLOAD_TOO_LARGE => {
                Self::new(status, "payload_too_large", "Request body too large")
            }
            StatusCode::UNSUPPORTED_MEDIA_TYPE => {
                Self::new(status, "unsupported_media_type", "Unsupported media type")
            }
            StatusCode::UNPROCESSABLE_ENTITY => Self::validation(Vec::new()),
            StatusCode::TOO_MANY_REQUESTS => Self::rate_limited(60),
            StatusCode::SERVICE_UNAVAILABLE => Self::unavailable(),
            s if s.is_client_error() => Self::new(s, "bad_request", "Bad request"),
            s => Self::new(s, "internal", "Internal error"),
        }
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let mut res = self.0.status.into_response();
        res.extensions_mut().insert(*self.0);
        res
    }
}

impl std::fmt::Display for AppError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} {}", self.0.status.as_u16(), self.0.code)
    }
}

/// RFC 9457 problem document.
#[derive(Debug, Serialize)]
pub struct Problem<'a> {
    #[serde(rename = "type")]
    pub kind: String,
    pub title: &'a str,
    pub status: u16,
    pub code: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<&'a str>,
    #[serde(skip_serializing_if = "<[FieldError]>::is_empty")]
    pub errors: &'a [FieldError],
    pub request_id: &'a str,
}

impl<'a> Problem<'a> {
    #[must_use]
    pub fn new(info: &'a ErrorInfo, request_id: &'a str) -> Self {
        Self {
            kind: format!("https://slath.org/problems/{}", info.code.replace('_', "-")),
            title: info.title,
            status: info.status.as_u16(),
            code: info.code,
            detail: info.detail.as_deref(),
            errors: &info.errors,
            request_id,
        }
    }
}

impl From<slath_service::ServiceError> for AppError {
    fn from(e: slath_service::ServiceError) -> Self {
        use slath_service::ServiceError as S;
        match e {
            S::NotFound => Self::not_found(),
            S::Forbidden => Self::forbidden(),
            S::Unauthenticated => Self::unauthenticated(),
            S::SudoRequired => Self::with_code(
                StatusCode::FORBIDDEN,
                "sudo_required",
                "Confirm your password",
            )
            .detail("This action needs a recent password confirmation."),
            S::Moved { namespace, name } => {
                Self::with_code(StatusCode::MOVED_PERMANENTLY, "moved", "Moved")
                    .detail(format!("{namespace}/{name}"))
            }
            S::Conflict(what) => Self::conflict(what),
            S::Invalid(errors) => Self::validation(
                errors
                    .into_iter()
                    .map(|f| FieldError {
                        field: f.field,
                        code: f.code,
                        message: f.message,
                    })
                    .collect(),
            ),
            S::RateLimited { retry_after } => Self::rate_limited(retry_after),
            S::Unavailable => Self::unavailable(),
            S::Internal(msg) => Self::internal(&msg),
        }
    }
}
