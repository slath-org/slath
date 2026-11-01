// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Shared setup of the service tests: services over a test database.
#![allow(dead_code, clippy::unwrap_used, clippy::expect_used)]

use std::sync::Arc;

use slath_core::authz::{Credential, Subject, UserKind, UserState};
use slath_core::ids::UserId;
use slath_service::Services;
use sqlx::PgPool;

pub struct Env {
    pub svc: Services,
    pub dir: tempfile::TempDir,
}

/// Services with a fresh master key and a temporary data directory.
pub fn services(pool: PgPool, extra_config: &str) -> Env {
    services_with_key(
        pool,
        extra_config,
        &slath_auth::MasterKey::generate().unwrap(),
        &[],
    )
}

pub fn services_with_key(
    pool: PgPool,
    extra_config: &str,
    master: &slath_auth::MasterKey,
    old: &[slath_auth::MasterKey],
) -> Env {
    let dir = tempfile::tempdir().unwrap();
    let data = dir.path().join("data");
    let home = data.join("git-home");
    std::fs::create_dir_all(&home).unwrap();
    let text = format!(
        "[server]\npublic_url = \"https://git.example.org\"\n[storage]\ndata_dir = \"{}\"\nrun_dir = \"{}\"\n{extra_config}",
        data.display(),
        dir.path().join("run").display()
    );
    let cfg: slath_core::config::Config = toml::from_str(&text).unwrap();
    let git = std::env::var_os("SLATH_TEST_GIT").map_or_else(|| "/usr/bin/git".into(), Into::into);
    let runner = slath_git::Runner::new(slath_git::RunnerConfig::new(git, home)).unwrap();
    let svc = Services::new(pool, Arc::new(cfg), master, runner).with_old_keys(master, old);
    Env { svc, dir }
}

/// Creates an active human user with a password and returns a session subject.
pub async fn user(svc: &Services, name: &str) -> (UserId, Subject) {
    let id = svc
        .create_account(
            &Subject::System,
            slath_service::accounts::NewAccount {
                username: name.to_owned(),
                email: Some(format!("{name}@example.org")),
                display_name: None,
                password: Some(slath_core::Secret::new("correct horse battery".to_owned())),
                is_admin: false,
            },
        )
        .await
        .unwrap();
    (id, session(id))
}

#[must_use]
pub fn session(id: UserId) -> Subject {
    Subject::User {
        id,
        kind: UserKind::Human,
        state: UserState::Active,
        is_site_admin: false,
        credential: Credential::Session,
    }
}
