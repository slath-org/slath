// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! HTTP surface of `slath serve` and the internal IPC endpoint.
//!
//! Git smart HTTP still serves the repositories listed in the config's
//! transport-test section and authenticates with one static token; it moves
//! onto database-backed repositories and tokens with the service layer.
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

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use axum::extract::DefaultBodyLimit;
use slath_core::Secret;
use slath_core::config::Config;
use slath_db::PgPool;
use slath_git::Runner;

/// A repository served by the spike, addressed as `/{namespace}/{name}`.
#[derive(Debug, Clone)]
pub struct RepoEntry {
    pub namespace: String,
    pub name: String,
    pub git_dir: PathBuf,
}

/// Shared state of the HTTP server. Cheap to clone.
#[derive(Clone)]
pub struct AppState {
    pub config: Arc<Config>,
    pub pool: PgPool,
    pub runner: Runner,
    pub assets: Arc<assets::Assets>,
    pub metrics: Arc<metrics::Metrics>,
    pub liveness: Arc<health::Liveness>,
    /// Keyed by lowercase `(namespace, name)`: names are case-insensitive.
    pub repos: Arc<HashMap<(String, String), RepoEntry>>,
    pub token: Arc<Secret<String>>,
    pub hooks: Arc<ipc::HookRegistry>,
    pub ipc_socket: PathBuf,
    pub services: slath_service::Services,
    pub trusted_proxies: Arc<auth::TrustedProxies>,
}

/// Everything [`AppState::new`] needs.
pub struct StateParts {
    pub config: Arc<Config>,
    pub pool: PgPool,
    pub runner: Runner,
    pub repos: Vec<RepoEntry>,
    pub token: Secret<String>,
    pub hooks: Arc<ipc::HookRegistry>,
    pub ipc_socket: PathBuf,
    pub services: slath_service::Services,
}

impl AppState {
    pub fn new(p: StateParts) -> Result<Self, String> {
        let trusted_proxies = auth::TrustedProxies::parse(&p.config.server.trusted_proxies)?;
        let repos = p
            .repos
            .into_iter()
            .map(|r| ((r.namespace.to_lowercase(), r.name.to_lowercase()), r))
            .collect();
        Ok(Self {
            trusted_proxies: Arc::new(trusted_proxies),
            services: p.services,
            config: p.config,
            pool: p.pool,
            runner: p.runner,
            assets: Arc::new(assets::Assets::embedded()),
            metrics: Arc::default(),
            liveness: Arc::default(),
            repos: Arc::new(repos),
            token: Arc::new(p.token),
            hooks: p.hooks,
            ipc_socket: p.ipc_socket,
        })
    }

    fn repo(&self, namespace: &str, name: &str) -> Option<&RepoEntry> {
        self.repos
            .get(&(namespace.to_lowercase(), name.to_lowercase()))
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
