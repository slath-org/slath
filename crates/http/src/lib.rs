// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! HTTP surface of `slath serve` and the internal IPC endpoint.
//!
//! Transport spike: smart HTTP serves repositories listed in the config and
//! authenticates with one static token. Authorization moves into `service`
//! later; until then [`githttp`] decides access itself.
#![forbid(unsafe_code)]

pub mod githttp;
pub mod ipc;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use slath_core::Secret;
use slath_git::Runner;

/// A repository served by the spike, addressed as `/{namespace}/{name}`.
#[derive(Debug, Clone)]
pub struct RepoEntry {
    pub namespace: String,
    pub name: String,
    pub git_dir: PathBuf,
}

/// Shared state of the HTTP server.
#[derive(Clone)]
pub struct AppState {
    pub runner: Runner,
    /// Keyed by lowercase `(namespace, name)`: names are case-insensitive.
    pub repos: Arc<HashMap<(String, String), RepoEntry>>,
    pub token: Arc<Secret<String>>,
    pub hooks: Arc<ipc::HookRegistry>,
    pub ipc_socket: PathBuf,
}

impl AppState {
    #[must_use]
    pub fn new(
        runner: Runner,
        repos: Vec<RepoEntry>,
        token: Secret<String>,
        hooks: Arc<ipc::HookRegistry>,
        ipc_socket: PathBuf,
    ) -> Self {
        let repos = repos
            .into_iter()
            .map(|r| ((r.namespace.to_lowercase(), r.name.to_lowercase()), r))
            .collect();
        Self {
            runner,
            repos: Arc::new(repos),
            token: Arc::new(token),
            hooks,
            ipc_socket,
        }
    }

    fn repo(&self, namespace: &str, name: &str) -> Option<&RepoEntry> {
        self.repos
            .get(&(namespace.to_lowercase(), name.to_lowercase()))
    }
}

/// The complete router.
pub fn router(state: AppState) -> axum::Router {
    githttp::router().with_state(state)
}
