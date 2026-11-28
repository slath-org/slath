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
pub mod agit;
pub mod apply;
pub mod attachments;
pub mod attention;
pub mod audit;
pub mod auto_merge;
pub mod avatars;
pub mod bgmigrations;
pub mod blobs;
pub mod bot_control;
pub mod bots;
pub mod brief_schema;
pub mod change_briefs;
pub mod change_sources;
pub mod context_records;
pub mod cross_refs;
pub mod deploy_keys;
pub mod email;
pub mod emails;
pub mod error;
mod fact_rules;
pub mod facts;
pub mod findings;
pub mod forks;
pub mod git_identities;
pub mod imports;
pub mod inventory;
pub mod issues;
pub mod iteration;
pub mod jobs;
pub mod keys;
pub mod labels;
pub mod landed;
pub mod logos;
pub mod maintainer_edits;
pub mod maintenance;
pub mod merge;
pub mod merge_requests;
pub mod mfa;
pub mod mr_branches;
pub mod mr_changes;
pub mod mr_groups;
pub mod mr_reviews;
pub mod mr_threads;
pub mod net;
pub mod notifications;
pub mod oidc;
pub mod one_maintainer;
pub mod orgs;
pub mod origins;
pub mod partial_landing;
pub mod passkeys;
pub mod path_classes;
pub mod protections;
pub mod push;
pub mod push_protection;
pub mod readiness;
pub mod reconcile;
pub mod recovery;
pub mod references;
pub mod registration;
pub mod repos;
mod review_requirements;
pub mod saved_views;
pub mod search;
pub mod secrets;
pub mod sensitive_paths;
pub mod sessions;
pub mod settings;
pub mod statuses;
pub mod suggestions;
pub mod teams;
pub mod tokens;
pub mod tools;
pub mod update_branch;
pub mod updates;
pub mod webhooks;
pub mod work_orders;

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
    /// Pushes per bot (`[limits] bot_pushes_per_hour`).
    pub bot_push_limiter: Arc<bot_control::PushLimiter>,
    /// Push protection's rules (built-in plus the configuration's).
    pub scan_rules: Arc<slath_core::secret_scan::RuleSet>,
    /// The names of all bots, cached.
    pub bot_names: Arc<bots::BotNames>,
    pub avatar_index: Arc<avatars::AvatarIndex>,
    /// The uploaded instance logos, cached.
    pub logo_cache: Arc<logos::LogoCache>,
    /// What Git says about recent diffs, for their facts.
    pub facts_cache: Arc<facts::FactsCache>,
}

/// One person's name as typed, pasted, or left by a picker: without
/// surrounding spaces, a leading `@`, or a trailing list separator ("@bob,"
/// is bob).
#[must_use]
pub fn one_name(s: &str) -> &str {
    s.trim()
        .trim_start_matches('@')
        .trim_end_matches(|c: char| c == ',' || c.is_whitespace())
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
            facts_cache: Arc::default(),
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
            bot_push_limiter: Arc::default(),
            oidc_cache: Arc::default(),
            bot_names: Arc::default(),
            avatar_index: Arc::default(),
            logo_cache: Arc::default(),
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

#[cfg(test)]
mod tests {
    #[test]
    fn one_name_drops_what_pickers_and_pastes_leave() {
        for s in ["bob", " bob ", "@bob", "bob,", "bob, ", " @bob ,"] {
            assert_eq!(super::one_name(s), "bob", "{s:?}");
        }
        assert_eq!(super::one_name("acme/team,"), "acme/team");
        assert_eq!(super::one_name(","), "");
    }
}
