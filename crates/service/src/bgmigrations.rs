// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Background migrations: the registry, the runner that works through them
//! while the server is up, and operator controls (CLI and admin UI).

use std::sync::Arc;

use slath_core::authz::Subject;
use slath_db::bgmigrations::{self as bg, BackgroundMigration, Control, Status, Step};

use crate::Services;
use crate::error::{Result, ServiceError};

/// Fails with [`ServiceError::Computing`] until the background migration
/// `name` has finished. Features that depend on a backfill call this first
/// and show a "computing" state instead of incomplete data.
#[macro_export]
macro_rules! requires_bgmigration {
    ($svc:expr, $name:expr) => {
        if !$svc.background_migration_done($name).await? {
            return Err($crate::ServiceError::Computing($name));
        }
    };
}

/// Re-applies the per-repository Git config (`slath_git::storage::REPO_CONFIG`)
/// to every repository after it changed. Its name carries the config
/// version, so bumping the version registers a new run. Writing the config
/// is idempotent, which a repeated batch relies on.
pub struct RepoConfigRefresh {
    name: &'static str,
    data_dir: std::path::PathBuf,
    runner: slath_git::Runner,
}

impl RepoConfigRefresh {
    #[must_use]
    pub fn new(data_dir: std::path::PathBuf, runner: slath_git::Runner) -> Self {
        // One name per version; leaked once per process (a handful of bytes).
        let name: &'static str = Box::leak(
            format!("repo_config_v{}", slath_git::storage::REPO_CONFIG_VERSION).into_boxed_str(),
        );
        Self {
            name,
            data_dir,
            runner,
        }
    }
}

impl BackgroundMigration for RepoConfigRefresh {
    fn name(&self) -> &'static str {
        self.name
    }

    fn introduced_in(&self) -> &'static str {
        "0.2.0"
    }

    fn estimate<'a>(&'a self, pool: &'a sqlx::PgPool) -> bg::EstimateFuture<'a> {
        Box::pin(async move {
            sqlx::query_scalar!(r#"SELECT count(*) AS "n!" FROM repositories"#)
                .fetch_one(pool)
                .await
                .map(Some)
                .map_err(|e| e.to_string())
        })
    }

    fn run_batch<'a>(
        &'a self,
        tx: &'a mut sqlx::PgConnection,
        cursor: Option<&'a serde_json::Value>,
        limit: i64,
    ) -> bg::BatchFuture<'a> {
        Box::pin(async move {
            let after = cursor
                .and_then(serde_json::Value::as_str)
                .and_then(|s| s.parse::<uuid::Uuid>().ok());
            let ids = sqlx::query_scalar!(
                "SELECT id FROM repositories WHERE ($1::uuid IS NULL OR id > $1) ORDER BY id LIMIT $2",
                after,
                limit
            )
            .fetch_all(&mut *tx)
            .await
            .map_err(|e| e.to_string())?;
            for id in &ids {
                let rid = slath_core::ids::RepoId::from_uuid(*id);
                let live = slath_git::storage::repo_path(&self.data_dir, rid);
                let trash = self.data_dir.join("trash").join(format!("{rid}.git"));
                for path in [live, trash] {
                    if path.is_dir() {
                        slath_git::storage::apply_config(&self.runner, &path)
                            .await
                            .map_err(|e| format!("repository {rid}: {e}"))?;
                    }
                }
            }
            let processed = i64::try_from(ids.len()).unwrap_or(i64::MAX);
            Ok(bg::Batch {
                cursor: (processed == limit)
                    .then(|| ids.last().map(|id| serde_json::Value::from(id.to_string())))
                    .flatten(),
                processed,
            })
        })
    }
}

fn admin_or_system(svc: &Services, subject: &Subject) -> Result<()> {
    match subject {
        Subject::System => Ok(()),
        _ => svc.require_site_admin(subject),
    }
}

impl Services {
    /// Adds a migration to the registry (tests, and migrations defined
    /// outside this crate).
    #[must_use]
    pub fn with_background_migration(mut self, m: Arc<dyn BackgroundMigration>) -> Self {
        let mut all = self.background_registry.as_ref().clone();
        all.push(m);
        self.background_registry = Arc::new(all);
        self
    }

    /// Every background migration of this release, oldest first.
    #[must_use]
    pub fn background_migrations(&self) -> Vec<Arc<dyn BackgroundMigration>> {
        self.background_registry.as_ref().clone()
    }

    /// Records every known migration as pending (idempotent). Run by
    /// `migrate`, `upgrade`, and `serve` at startup. Returns the names of
    /// newly registered ones.
    pub async fn register_background_migrations(&self) -> Result<Vec<&'static str>> {
        let mut new = Vec::new();
        for m in self.background_migrations() {
            if bg::register(&self.pool, m.as_ref()).await? {
                new.push(m.name());
            }
        }
        Ok(new)
    }

    /// True once `name` has finished.
    pub async fn background_migration_done(&self, name: &str) -> Result<bool> {
        Ok(bg::is_done(&self.pool, name).await?)
    }

    /// Works through pending migrations one batch at a time until none is
    /// left or `stop` is set. Several servers may run this at once; a
    /// row lock lets only one of them run a given batch.
    pub async fn run_background_migrations(
        &self,
        stop: &(dyn Fn() -> bool + Send + Sync),
    ) -> Result<()> {
        let limits = &self.config.limits;
        for m in self.background_migrations() {
            match bg::run_to_end(
                &self.pool,
                m.as_ref(),
                i64::from(limits.background_migration_batch),
                limits.background_migration_interval.get(),
                stop,
            )
            .await?
            {
                Step::Failed(e) => {
                    tracing::error!(migration = m.name(), error = %e, "background migration failed");
                }
                Step::Progress { done: true, .. } => {
                    tracing::info!(migration = m.name(), "background migration finished");
                }
                _ => {}
            }
            if stop() {
                break;
            }
        }
        Ok(())
    }

    /// Status of every migration (site administrators and the system).
    pub async fn background_migration_status(&self, subject: &Subject) -> Result<Vec<Status>> {
        admin_or_system(self, subject)?;
        Ok(bg::list(&self.pool).await?)
    }

    /// The most recent upgrades (site administrators).
    pub async fn upgrade_history(
        &self,
        subject: &Subject,
    ) -> Result<Vec<slath_db::bgmigrations::UpgradeRecord>> {
        admin_or_system(self, subject)?;
        Ok(bg::upgrade_history(&self.pool, 50).await?)
    }

    /// Pauses, resumes, or retries a migration; audited.
    pub async fn control_background_migration(
        &self,
        subject: &Subject,
        name: &str,
        c: Control,
    ) -> Result<()> {
        admin_or_system(self, subject)?;
        if !bg::control(&self.pool, name, c).await? {
            return Err(match bg::get(&self.pool, name).await? {
                None => ServiceError::NotFound,
                Some(s) => ServiceError::Conflict(format!(
                    "{name} is {}; that isn't possible now",
                    s.state.as_str()
                )),
            });
        }
        let action = match c {
            Control::Pause => "background_migration.pause",
            Control::Resume => "background_migration.resume",
            Control::Retry => "background_migration.retry",
        };
        self.audit(
            subject,
            action,
            crate::audit::Target::new("background_migration", None),
            serde_json::json!({"name": name}),
        )
        .await;
        Ok(())
    }
}
