// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Recording audit events. Callers pass names and IDs only; secrets
//! (passwords, tokens, keys' private parts) never go into event data.

use slath_core::authz::Subject;

use crate::Services;

tokio::task_local! {
    /// The client IP of the request being handled, set by the HTTP layer.
    pub static CLIENT_IP: Option<String>;
}

/// Runs `fut` with `ip` as the client IP for audit events.
pub async fn with_client_ip<F: std::future::Future>(ip: Option<String>, fut: F) -> F::Output {
    CLIENT_IP.scope(ip, fut).await
}

/// What an event is about.
#[derive(Debug, Clone, Copy)]
pub struct Target {
    pub kind: &'static str,
    pub id: Option<uuid::Uuid>,
    pub repo_id: Option<uuid::Uuid>,
    pub org_id: Option<uuid::Uuid>,
}

impl Target {
    #[must_use]
    pub const fn new(kind: &'static str, id: Option<uuid::Uuid>) -> Self {
        Self {
            kind,
            id,
            repo_id: None,
            org_id: None,
        }
    }

    #[must_use]
    pub const fn repo(id: uuid::Uuid) -> Self {
        Self {
            kind: "repository",
            id: Some(id),
            repo_id: Some(id),
            org_id: None,
        }
    }

    /// Also records the repository the target belongs to.
    #[must_use]
    pub const fn in_repo(mut self, repo: uuid::Uuid) -> Self {
        self.repo_id = Some(repo);
        self
    }

    #[must_use]
    pub const fn org(id: uuid::Uuid) -> Self {
        Self {
            kind: "organization",
            id: Some(id),
            repo_id: None,
            org_id: Some(id),
        }
    }
}

impl Services {
    /// Records one audit event. Failures are logged, never propagated: an
    /// audit outage must not turn a completed action into an error.
    pub async fn audit(
        &self,
        actor: &Subject,
        action: &str,
        target: Target,
        data: serde_json::Value,
    ) {
        let actor_id = match actor {
            Subject::User { id, .. } => Some(*id.as_uuid()),
            _ => None,
        };
        let ip = CLIENT_IP.try_with(Clone::clone).ok().flatten();
        let event = slath_db::audit::NewEvent {
            actor_id,
            actor_ip: ip.as_deref(),
            action,
            target_kind: target.kind,
            target_id: target.id,
            repo_id: target.repo_id,
            org_id: target.org_id,
            data,
        };
        if let Err(e) = slath_db::audit::insert(&self.pool, &event).await {
            tracing::error!(error = %e, action, "recording an audit event failed");
        }
    }

    /// Deletes audit events past `maintenance.audit_retention_days`.
    pub async fn purge_audit(&self) -> crate::Result<u64> {
        let days = i32::try_from(self.config.maintenance.audit_retention_days).unwrap_or(i32::MAX);
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(crate::ServiceError::internal)?;
        let n = slath_db::audit::purge(&mut tx, days).await?;
        tx.commit().await.map_err(crate::ServiceError::internal)?;
        Ok(n)
    }
}
