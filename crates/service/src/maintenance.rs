// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Read-only maintenance mode. `slath upgrade` sets a flag in the database
//! (so nothing but database access can switch it); every server polls it
//! and then refuses changes: pages and API writes answer 503, pushes are
//! rejected with a message, and reads keep working.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};

use slath_db::settings::MAINTENANCE;

use crate::Services;
use crate::error::Result;

/// The flag as this server last saw it.
#[derive(Debug, Default)]
pub struct MaintenanceState {
    on: AtomicBool,
    reason: RwLock<String>,
}

impl MaintenanceState {
    #[must_use]
    pub fn is_on(&self) -> bool {
        self.on.load(Ordering::Relaxed)
    }

    /// Why (shown to users), if on.
    #[must_use]
    pub fn reason(&self) -> Option<String> {
        self.is_on()
            .then(|| self.reason.read().map(|r| r.clone()).unwrap_or_default())
    }

    fn update(&self, reason: Option<String>) {
        if let Ok(mut r) = self.reason.write() {
            *r = reason.clone().unwrap_or_default();
        }
        self.on.store(reason.is_some(), Ordering::Relaxed);
    }
}

impl Services {
    /// The maintenance state shared by this server's surfaces.
    #[must_use]
    pub fn maintenance(&self) -> &Arc<MaintenanceState> {
        &self.maintenance
    }

    /// Reads the flag from the database into [`Services::maintenance`].
    pub async fn refresh_maintenance(&self) -> Result<bool> {
        let v = slath_db::settings::get(&self.pool, MAINTENANCE).await?;
        let reason = v.map(|v| {
            v.get("reason")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("maintenance")
                .to_owned()
        });
        self.maintenance.update(reason);
        Ok(self.maintenance.is_on())
    }

    /// Turns maintenance on for every server (CLI only). With
    /// `until_version`, a server of at least that version clears it when it
    /// starts and `finished` is set (an upgrade waiting for the restart).
    pub async fn enter_maintenance(
        &self,
        reason: &str,
        until_version: Option<&str>,
        finished: bool,
    ) -> Result<()> {
        slath_db::settings::set(
            &self.pool,
            MAINTENANCE,
            &serde_json::json!({
                "reason": reason,
                "since": time::OffsetDateTime::now_utc().unix_timestamp(),
                "until_version": until_version,
                "finished": finished,
            }),
        )
        .await?;
        self.maintenance.update(Some(reason.to_owned()));
        Ok(())
    }

    /// Turns maintenance off.
    pub async fn leave_maintenance(&self) -> Result<()> {
        slath_db::settings::delete(&self.pool, MAINTENANCE).await?;
        self.maintenance.update(None);
        Ok(())
    }
}

/// `a <= b` for dotted numeric versions (pre-release suffixes ignored).
fn version_le(a: &str, b: &str) -> bool {
    let parse = |v: &str| -> Vec<u64> {
        v.split('-')
            .next()
            .unwrap_or("")
            .split('.')
            .map(|p| p.parse().unwrap_or(0))
            .collect()
    };
    parse(a) <= parse(b)
}

impl Services {
    /// At server start: records which version serves the database, and ends
    /// the maintenance of a finished upgrade to this version (or older).
    pub async fn on_server_start(&self) -> Result<()> {
        let me = env!("CARGO_PKG_VERSION");
        slath_db::settings::set(
            &self.pool,
            slath_db::settings::SERVED_BY,
            &serde_json::Value::from(me),
        )
        .await?;
        if let Some(v) = slath_db::settings::get(&self.pool, MAINTENANCE).await? {
            let finished = v.get("finished").and_then(serde_json::Value::as_bool) == Some(true);
            let until = v.get("until_version").and_then(serde_json::Value::as_str);
            if finished && until.is_some_and(|u| version_le(u, me)) {
                self.leave_maintenance().await?;
                tracing::info!("upgrade finished; maintenance mode ended");
            }
        }
        self.refresh_maintenance().await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn versions() {
        assert!(super::version_le("0.2.0", "0.2.0"));
        assert!(super::version_le("0.2.0", "0.10.0"));
        assert!(!super::version_le("0.3.0", "0.2.9"));
        assert!(super::version_le("0.2.0-rc.1", "0.2.0"));
    }
}
