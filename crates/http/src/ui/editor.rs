// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Support for the Markdown editor: the server-rendered preview shown in
//! the editor's Preview tab, and the remembered editor mode.

use axum::body::Bytes;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::{Form, Router};
use serde::Deserialize;
use slath_core::authz::Action;
use slath_markup::refs::RefContext;

use crate::AppState;
use crate::auth::{MaybeSession, RequireSession, form_pairs};
use crate::error::AppError;
use crate::routes::{Access, RouteExt};

#[derive(Deserialize, Default)]
struct PreviewQuery {
    /// `owner/repo` the text belongs to (for `#12`, `~label`, …).
    #[serde(default)]
    repo: String,
    /// The form field holding the text (default `body`).
    #[serde(default)]
    field: String,
}

/// `POST /-/ui/markdown/preview`: renders the posted text as the server
/// will show it to the viewer.
async fn preview(
    State(state): State<AppState>,
    session: MaybeSession,
    Query(q): Query<PreviewQuery>,
    body: Bytes,
) -> Result<Response, AppError> {
    let field = if q.field.is_empty() {
        "body"
    } else {
        q.field.as_str()
    };
    let text = form_pairs(&body)
        .into_iter()
        .find(|(k, _)| k == field || k == "text")
        .map(|(_, v)| v)
        .unwrap_or_default();
    let max = state.config.limits.text_max_bytes.as_usize();
    if text.len() > max {
        return Err(AppError::with_code(
            StatusCode::PAYLOAD_TOO_LARGE,
            "too_large",
            "Text too long",
        ));
    }
    let subject = session.subject();
    // References resolve against the repository only if the viewer can read it.
    let rc = match q.repo.split_once('/') {
        Some((ns, repo)) => state
            .services
            .repo_access(&subject, ns, repo, Action::RepoRead)
            .await
            .ok()
            .map(|a| RefContext {
                ns: a.repo.namespace.clone(),
                repo: a.repo.name.clone(),
            }),
        None => None,
    };
    let html = state
        .services
        .render_markdown(&subject, rc.as_ref(), &text)
        .await?;
    let out = if html.trim().is_empty() {
        "<p class=\"text-secondary\">Nothing to preview.</p>".to_owned()
    } else {
        html
    };
    Ok(([("content-type", "text/html; charset=utf-8")], out).into_response())
}

#[derive(Deserialize)]
struct ModeForm {
    #[serde(default)]
    mode: String,
}

/// `POST /-/settings/editor-mode`: the editor remembers the last mode.
async fn set_mode(
    State(state): State<AppState>,
    RequireSession(s): RequireSession,
    Form(f): Form<ModeForm>,
) -> Result<StatusCode, AppError> {
    state.services.set_editor_mode(&s, &f.mode).await?;
    Ok(StatusCode::NO_CONTENT)
}

pub fn router() -> Router<AppState> {
    use axum::routing::post;
    Router::new()
        .route_as("/-/ui/markdown/preview", Access::Public, post(preview))
        .route_as("/-/settings/editor-mode", Access::Account, post(set_mode))
}
