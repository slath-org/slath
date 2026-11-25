// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Content trust on the way out: every JSON response to a task token whose
//! order limits whose text it reads passes through the order's filter,
//! after the operation ran. The MCP tools run their operations through the
//! same layer, so their results are filtered the same way.

use axum::Json;
use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{Method, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use slath_core::authz::{Credential, Subject};

use super::ApiSubject;
use crate::AppState;
use crate::error::AppError;

/// Filters the response for task tokens under content trust.
pub async fn content_trust(State(state): State<AppState>, req: Request, next: Next) -> Response {
    let Some(ApiSubject(subject)) = req.extensions().get::<ApiSubject>().cloned() else {
        return next.run(req).await;
    };
    if !matches!(
        &subject,
        Subject::User {
            credential: Credential::Task { .. },
            ..
        }
    ) {
        return next.run(req).await;
    }
    let filter = match state.services.content_trust_filter(&subject).await {
        Ok(Some(f)) => f,
        Ok(None) => return next.run(req).await,
        Err(e) => return AppError::from(e).into_response(),
    };
    if matches!(*req.method(), Method::GET | Method::HEAD)
        && let Some(p) = mr_in_path(req.uri().path(), &filter.namespace, &filter.repo)
            .and_then(|n| filter.mr_placeholder(n))
    {
        return Json(p).into_response();
    }
    let res = next.run(req).await;
    let is_json = res
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|t| t.starts_with("application/json"));
    if !is_json {
        return res;
    }
    let (mut parts, body) = res.into_parts();
    let redacted = async {
        let bytes = axum::body::to_bytes(body, usize::MAX)
            .await
            .map_err(|e| e.to_string())?;
        let mut v: serde_json::Value = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
        filter.redact(&mut v);
        serde_json::to_vec(&v).map_err(|e| e.to_string())
    };
    match redacted.await {
        Ok(out) => {
            parts.headers.remove(header::CONTENT_LENGTH);
            parts.headers.remove(header::ETAG);
            Response::from_parts(parts, Body::from(out))
        }
        // Never pass on what couldn't be filtered.
        Err(e) => AppError::internal(&e).into_response(),
    }
}

/// The merge request number in `/api/v1/repos/{ns}/{repo}/merge-requests/{n}…`
/// when the path is in the order's repository.
fn mr_in_path(path: &str, ns: &str, repo: &str) -> Option<i32> {
    let mut seg = path.strip_prefix("/api/v1/repos/")?.split('/');
    let (n, r) = (seg.next()?, seg.next()?);
    if !n.eq_ignore_ascii_case(ns) || !r.eq_ignore_ascii_case(repo) {
        return None;
    }
    if seg.next()? != "merge-requests" {
        return None;
    }
    // `7`, and `7.diff` should such a form ever exist.
    let n = seg.next()?;
    let digits = n.find(|c: char| !c.is_ascii_digit()).map_or(n, |i| &n[..i]);
    digits.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_request_paths() {
        let p = |s| mr_in_path(s, "acme", "app");
        assert_eq!(p("/api/v1/repos/acme/app/merge-requests/7"), Some(7));
        assert_eq!(
            p("/api/v1/repos/ACME/app/merge-requests/7/iteration"),
            Some(7)
        );
        assert_eq!(p("/api/v1/repos/acme/app/merge-requests/7.diff"), Some(7));
        assert_eq!(p("/api/v1/repos/acme/app/merge-requests"), None);
        assert_eq!(p("/api/v1/repos/acme/other/merge-requests/7"), None);
        assert_eq!(p("/api/v1/repos/acme/app/issues/7"), None);
    }
}
