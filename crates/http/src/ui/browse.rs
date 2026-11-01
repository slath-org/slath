// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! The code browser.

use axum::response::Response;
use slath_core::authz::Subject;
use slath_service::repos::RepoAccess;

use super::PageCtx;
use crate::AppState;
use crate::error::AppError;

/// The repository home of a repository with content.
pub async fn repo_home_with_content(
    state: AppState,
    ctx: PageCtx,
    subject: Subject,
    a: RepoAccess,
) -> Result<Response, AppError> {
    let (https_url, ssh_url) = super::repos::clone_urls(&state, &a.repo.namespace, &a.repo.name);
    let page = super::repos::non_empty_home(&a, &subject, ctx, https_url, ssh_url);
    Ok(super::render(&page))
}
