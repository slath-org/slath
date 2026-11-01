// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Use cases with built-in authorization.
//!
//! Every public operation takes the acting [`Subject`] first and decides
//! itself whether the subject may perform it. Callers (HTTP handlers, the
//! IPC server, the CLI) never query repository-scoped data directly.
#![forbid(unsafe_code)]

pub mod accounts;
pub mod error;
pub mod sessions;

use std::sync::Arc;

pub use error::{FieldError, Result, ServiceError};
use slath_auth::csrf::Csrf;
use slath_core::config::Config;
use slath_db::PgPool;

/// Shared state of the use cases. Cheap to clone.
#[derive(Clone)]
pub struct Services {
    pub pool: PgPool,
    pub config: Arc<Config>,
    pub csrf: Arc<Csrf>,
}

impl Services {
    #[must_use]
    pub fn new(pool: PgPool, config: Arc<Config>, master: &slath_auth::MasterKey) -> Self {
        Self {
            pool,
            config,
            csrf: Arc::new(Csrf::new(master.derive("csrf"))),
        }
    }
}
