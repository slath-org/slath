// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! The error type of every use case.
//!
//! A resource the subject may not see is reported as [`ServiceError::NotFound`],
//! never [`ServiceError::Forbidden`]: that mapping happens here, in the
//! service layer, so no surface can leak existence by accident.

use serde::Serialize;

/// One field-level validation problem.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FieldError {
    pub field: String,
    /// Stable machine-readable code, e.g. `invalid_chars`.
    pub code: String,
    /// Message for people (shown next to the field).
    #[serde(skip_serializing_if = "String::is_empty")]
    pub message: String,
}

impl FieldError {
    #[must_use]
    pub fn new(field: &str, code: &str, message: impl Into<String>) -> Self {
        Self {
            field: field.to_owned(),
            code: code.to_owned(),
            message: message.into(),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ServiceError {
    #[error("not found")]
    NotFound,
    /// Only for resources the subject can see.
    #[error("forbidden")]
    Forbidden,
    /// Authentication is required (or the credentials are invalid).
    #[error("unauthenticated")]
    Unauthenticated,
    /// The action needs a recent re-authentication (sudo mode).
    #[error("sudo mode required")]
    SudoRequired,
    #[error("conflict: {0}")]
    Conflict(String),
    #[error("validation failed")]
    Invalid(Vec<FieldError>),
    #[error("rate limited")]
    RateLimited { retry_after: u64 },
    #[error("unavailable")]
    Unavailable,
    /// Logged with the request ID; never shown to clients.
    #[error("internal error: {0}")]
    Internal(String),
}

impl ServiceError {
    #[must_use]
    pub fn invalid(field: &str, code: &str, message: impl Into<String>) -> Self {
        Self::Invalid(vec![FieldError::new(field, code, message)])
    }

    pub fn internal(e: impl std::fmt::Display) -> Self {
        Self::Internal(e.to_string())
    }
}

impl From<slath_db::DbError> for ServiceError {
    fn from(e: slath_db::DbError) -> Self {
        match e {
            slath_db::DbError::Conflict(field) => Self::Conflict(field.to_owned()),
            slath_db::DbError::Sqlx(sqlx::Error::PoolTimedOut) => Self::Unavailable,
            other => Self::Internal(other.to_string()),
        }
    }
}

pub type Result<T, E = ServiceError> = std::result::Result<T, E>;
