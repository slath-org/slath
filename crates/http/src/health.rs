// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! `GET /-/healthz` (the process is alive) and `GET /-/readyz` (it can serve:
//! database reachable with the expected schema, `data_dir` writable, job
//! workers running).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};

use crate::AppState;

/// Set by the worker pool while it runs.
#[derive(Debug, Default)]
pub struct Liveness {
    pub workers: AtomicBool,
}

pub async fn healthz() -> Response {
    (StatusCode::OK, [("cache-control", "no-store")], "ok\n").into_response()
}

async fn check(state: &AppState) -> Result<(), String> {
    let db = async {
        slath_db::check_schema(&state.pool)
            .await
            .map_err(|e| format!("database: {e}"))
    };
    tokio::time::timeout(Duration::from_secs(2), db)
        .await
        .map_err(|_| "database: timeout".to_owned())??;
    let dir = state.config.storage.data_dir.join("tmp");
    let probe = dir.join(format!(".readyz-{}", std::process::id()));
    tokio::fs::create_dir_all(&dir)
        .await
        .and(tokio::fs::write(&probe, b"ok").await)
        .map_err(|e| format!("data_dir: {e}"))?;
    let _ = tokio::fs::remove_file(&probe).await;
    if !state.liveness.workers.load(Ordering::Relaxed) {
        return Err("workers: not running".to_owned());
    }
    Ok(())
}

pub async fn readyz(State(state): State<AppState>) -> Response {
    match check(&state).await {
        Ok(()) => (StatusCode::OK, [("cache-control", "no-store")], "ready\n").into_response(),
        Err(why) => {
            tracing::warn!(reason = %why, "not ready");
            // The reason is operational detail; keep it out of the response.
            (
                StatusCode::SERVICE_UNAVAILABLE,
                [("cache-control", "no-store")],
                "not ready\n",
            )
                .into_response()
        }
    }
}

/// Marks the workers as alive until the returned guard is dropped.
#[must_use]
pub fn workers_running(l: &Arc<Liveness>) -> impl Drop + use<> {
    struct Guard(Arc<Liveness>);
    impl Drop for Guard {
        fn drop(&mut self) {
            self.0.workers.store(false, Ordering::Relaxed);
        }
    }
    l.workers.store(true, Ordering::Relaxed);
    Guard(Arc::clone(l))
}

/// Largest CSP report body we look at.
const MAX_REPORT: usize = 16 * 1024;

/// `POST /-/csp-report`: browsers report CSP violations here (both the
/// legacy `report-uri` and the Reporting API format). Reports are logged with
/// the violated directive only; the body is attacker-controlled, so nothing
/// else from it is kept, and it is capped in size.
pub async fn csp_report(body: axum::body::Bytes) -> Response {
    let body = &body[..body.len().min(MAX_REPORT)];
    let directive = serde_json::from_slice::<serde_json::Value>(body)
        .ok()
        .and_then(|v| {
            let report = v
                .get("csp-report")
                .or_else(|| v.get(0).and_then(|r| r.get("body")))?;
            report
                .get("violated-directive")
                .or_else(|| report.get("effectiveDirective"))
                .and_then(|d| d.as_str())
                .map(|d| {
                    d.chars()
                        .filter(|c| c.is_ascii_graphic())
                        .take(64)
                        .collect::<String>()
                })
        });
    tracing::warn!(
        directive = directive.as_deref().unwrap_or("unknown"),
        "csp violation reported"
    );
    StatusCode::NO_CONTENT.into_response()
}
