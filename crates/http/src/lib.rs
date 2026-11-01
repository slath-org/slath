// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! HTTP surface of `slath serve` and the internal IPC endpoint.
//!
#![forbid(unsafe_code)]

pub mod api;
pub mod assets;
pub mod auth;
pub mod error;
pub mod githttp;
pub mod health;
pub mod ipc;
pub mod metrics;
pub mod middleware;
pub mod ui;

use std::path::PathBuf;
use std::sync::Arc;

use axum::extract::DefaultBodyLimit;
use slath_core::config::Config;
use slath_db::PgPool;
use slath_git::Runner;

/// Shared state of the HTTP server. Cheap to clone.
#[derive(Clone)]
pub struct AppState {
    pub config: Arc<Config>,
    pub pool: PgPool,
    pub runner: Runner,
    pub assets: Arc<assets::Assets>,
    pub metrics: Arc<metrics::Metrics>,
    pub liveness: Arc<health::Liveness>,
    pub hooks: Arc<ipc::HookRegistry>,
    pub ipc_socket: PathBuf,
    pub services: slath_service::Services,
    pub trusted_proxies: Arc<auth::TrustedProxies>,
    /// API rate limits per principal.
    pub api_limits: Arc<api::support::RateLimiter>,
    /// Limits concurrent archive downloads (`limits.archive_max_concurrent`).
    pub archives: Arc<tokio::sync::Semaphore>,
}

/// Everything [`AppState::new`] needs.
pub struct StateParts {
    pub config: Arc<Config>,
    pub pool: PgPool,
    pub runner: Runner,
    pub hooks: Arc<ipc::HookRegistry>,
    pub ipc_socket: PathBuf,
    pub services: slath_service::Services,
}

impl AppState {
    pub fn new(p: StateParts, metrics: Arc<metrics::Metrics>) -> Result<Self, String> {
        let trusted_proxies = auth::TrustedProxies::parse(&p.config.server.trusted_proxies)?;
        Ok(Self {
            api_limits: Arc::default(),
            archives: Arc::new(tokio::sync::Semaphore::new(
                p.config.limits.archive_max_concurrent.max(1),
            )),
            trusted_proxies: Arc::new(trusted_proxies),
            services: p.services,
            config: p.config,
            pool: p.pool,
            runner: p.runner,
            assets: Arc::new(assets::Assets::embedded()),
            metrics,
            liveness: Arc::default(),
            hooks: p.hooks,
            ipc_socket: p.ipc_socket,
        })
    }
}

/// The complete router.
pub fn router(state: AppState) -> axum::Router {
    use axum::middleware::from_fn_with_state;
    use axum::routing::get;
    let body_limit = state.config.limits.http_max_body_bytes.as_usize();
    let pages = ui::router()
        .fallback(ui::fallback)
        .layer(from_fn_with_state(state.clone(), auth::csrf))
        .layer(from_fn_with_state(state.clone(), auth::resolve))
        .layer(from_fn_with_state(state.clone(), middleware::render_errors))
        .layer(DefaultBodyLimit::max(body_limit));
    let api = api::router()
        .layer(from_fn_with_state(state.clone(), api::support::api_context))
        .layer(from_fn_with_state(state.clone(), middleware::render_errors))
        .layer(DefaultBodyLimit::max(body_limit));
    let probes = axum::Router::new()
        .route("/-/healthz", get(health::healthz))
        .route("/-/readyz", get(health::readyz))
        .route(
            "/-/csp-report",
            axum::routing::post(health::csp_report).layer(DefaultBodyLimit::max(64 * 1024)),
        );
    axum::Router::new()
        .merge(githttp::router())
        .merge(probes)
        .merge(api)
        .merge(pages)
        .layer(from_fn_with_state(
            state.clone(),
            middleware::security_headers,
        ))
        .layer(from_fn_with_state(
            state.clone(),
            middleware::request_context,
        ))
        .with_state(state)
}
