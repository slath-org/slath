// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Use cases with built-in authorization.
//!
//! Every public operation takes the acting [`Subject`] first and decides
//! itself whether the subject may perform it. Callers (HTTP handlers, the
//! IPC server, the CLI) never query repository-scoped data directly.
#![forbid(unsafe_code)]

pub mod accounts;
pub mod admin;
pub mod attachments;
pub mod audit;
pub mod avatars;
pub mod bgmigrations;
pub mod blobs;
pub mod email;
pub mod error;
pub mod imports;
pub mod issues;
pub mod jobs;
pub mod keys;
pub mod labels;
pub mod maintenance;
pub mod merge_requests;
pub mod mfa;
pub mod mr_changes;
pub mod net;
pub mod notifications;
pub mod oidc;
pub mod orgs;
pub mod passkeys;
pub mod protections;
pub mod push;
pub mod push_protection;
pub mod reconcile;
pub mod recovery;
pub mod references;
pub mod registration;
pub mod repos;
pub mod search;
pub mod secrets;
pub mod sessions;
pub mod settings;
pub mod teams;
pub mod tokens;
pub mod updates;

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
    /// Signs API pagination cursors (an HMAC like the CSRF tokens).
    pub cursors: Arc<Csrf>,
    pub runner: slath_git::Runner,
    /// Encrypts and decrypts secrets at rest.
    pub envelope: Arc<slath_auth::Envelope>,
    /// Background migrations of this release, in order.
    pub background_registry: Arc<Vec<Arc<dyn slath_db::bgmigrations::BackgroundMigration>>>,
    /// Read-only maintenance as last seen in the database.
    pub maintenance: Arc<maintenance::MaintenanceState>,
    /// The WebAuthn relying party (`None`: passkeys unavailable here).
    pub webauthn: Option<Arc<webauthn_rs::prelude::Webauthn>>,
    /// Attachments and avatars.
    pub blobs: blobs::LocalBlobStore,
    /// Signs unsubscribe links in notification emails.
    pub unsubscribe_keys: Arc<Csrf>,
    /// OpenID provider metadata.
    pub oidc_cache: Arc<oidc::OidcCache>,
    /// Requests for reference suggestions per user.
    pub reference_limiter: Arc<references::Limiter>,
    /// Push protection's rules (built-in plus the configuration's).
    pub scan_rules: Arc<slath_core::secret_scan::RuleSet>,
}

impl Services {
    #[must_use]
    pub fn new(
        pool: PgPool,
        config: Arc<Config>,
        master: &slath_auth::MasterKey,
        runner: slath_git::Runner,
    ) -> Self {
        let webauthn = passkeys::relying_party(&config.server.public_url).map(Arc::new);
        let repo_config: Arc<dyn slath_db::bgmigrations::BackgroundMigration> = Arc::new(
            bgmigrations::RepoConfigRefresh::new(config.storage.data_dir.clone(), runner.clone()),
        );
        let blobs = blobs::LocalBlobStore::new(&config.storage.data_dir);
        // Loading the configuration already checked the operator's rules.
        let scan_rules = Arc::new(
            config
                .push_protection
                .rule_set()
                .unwrap_or_else(|_| slath_core::secret_scan::RuleSet::built_in()),
        );
        Self {
            scan_rules,
            pool,
            config,
            csrf: Arc::new(Csrf::new(master.derive("csrf"))),
            cursors: Arc::new(Csrf::new(master.derive("api-cursor"))),
            unsubscribe_keys: Arc::new(Csrf::new(master.derive("unsubscribe"))),
            runner,
            envelope: Arc::new(slath_auth::Envelope::new(master, &[])),
            background_registry: Arc::new(vec![repo_config]),
            maintenance: Arc::default(),
            webauthn,
            blobs,
            reference_limiter: Arc::default(),
            oidc_cache: Arc::default(),
        }
    }

    /// Adds old master keys (`secrets.old_key_files`) for decrypting values
    /// that haven't been rotated yet.
    #[must_use]
    pub fn with_old_keys(
        mut self,
        master: &slath_auth::MasterKey,
        old: &[slath_auth::MasterKey],
    ) -> Self {
        self.envelope = Arc::new(slath_auth::Envelope::new(master, old));
        self
    }
}
