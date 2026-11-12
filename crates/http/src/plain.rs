// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Plain-text diffs and patches of commits and merge requests, for tools
//! and agents: `…/commit/{oid}.diff` and `.patch`,
//! `…/merge-requests/{n}.diff` and `.patch` in the web UI, and the same as
//! `…/diff` and `…/patch` under the API.

use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use slath_core::authz::{Subject, Visibility};
use slath_git::ObjectId;
use slath_git::browse;
use slath_git::object::ObjectReader;
use slath_service::repos::RepoAccess;

use crate::AppState;
use crate::error::AppError;

/// Largest diff or patch served.
const MAX_BYTES: usize = 64 * 1024 * 1024;

/// What to send.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    /// A unified diff (`git diff`).
    Diff,
    /// The commits as mails (`git format-patch`).
    Patch,
}

/// Splits `.diff` or `.patch` off a path segment.
#[must_use]
pub fn split_suffix(segment: &str) -> (&str, Option<Format>) {
    if let Some(s) = segment.strip_suffix(".diff") {
        (s, Some(Format::Diff))
    } else if let Some(s) = segment.strip_suffix(".patch") {
        (s, Some(Format::Patch))
    } else {
        (segment, None)
    }
}

fn respond(data: Vec<u8>, too_big: bool, visibility: Visibility) -> Response {
    if too_big {
        return AppError::with_code(StatusCode::PAYLOAD_TOO_LARGE, "too_large", "Diff too large")
            .into_response();
    }
    let mut res = (StatusCode::OK, data).into_response();
    let h = res.headers_mut();
    h.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/plain; charset=utf-8"),
    );
    h.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static("sandbox; default-src 'none'"),
    );
    h.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    let cache = if visibility == Visibility::Public {
        "no-cache"
    } else {
        "private, no-store"
    };
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static(cache));
    res
}

/// A commit's diff against its first parent, or the commit as a patch.
/// `a` must already allow reading the repository.
pub async fn commit(
    state: &AppState,
    a: &RepoAccess,
    oid: &str,
    format: Format,
) -> Result<Response, AppError> {
    let git_dir = state.services.repo_path(a.repo.id);
    let oid = ObjectId::parse(oid.as_bytes()).map_err(|_| AppError::not_found())?;
    let oid = browse::resolve_commit(&state.runner, &git_dir, oid.as_str().as_bytes())
        .await
        .map_err(|e| AppError::internal(&e))?
        .ok_or_else(AppError::not_found)?;
    let mut reader = ObjectReader::open(&state.runner, &git_dir)
        .await
        .map_err(|e| AppError::internal(&e))?;
    let c = reader
        .read_raw_commit(&oid)
        .await
        .map_err(|e| AppError::internal(&e))?;
    let parent = c.parents().find_map(|p| ObjectId::parse(p).ok());
    let (data, too_big) = match format {
        Format::Diff => {
            browse::diff(
                &state.runner,
                &git_dir,
                parent.as_ref(),
                &oid,
                browse::DiffOptions::default(),
                MAX_BYTES,
            )
            .await
        }
        Format::Patch => {
            browse::format_patch(
                &state.runner,
                &git_dir,
                None,
                &oid,
                parent.is_none(),
                MAX_BYTES,
            )
            .await
        }
    }
    .map_err(|e| AppError::internal(&e))?;
    Ok(respond(data, too_big, a.repo.visibility))
}

/// A merge request's current revision: its diff from the merge base, or
/// its commits as patches.
pub async fn merge_request(
    state: &AppState,
    subject: &Subject,
    (ns, repo, number): (&str, &str, i32),
    format: Format,
) -> Result<Response, AppError> {
    let (a, m) = state.services.mr(subject, ns, repo, number).await?;
    let out = match format {
        Format::Diff => state
            .services
            .mr_diff(&m, None, browse::DiffOptions::default())
            .await?
            .map(|(d, cut, _)| (d, cut)),
        Format::Patch => state.services.mr_patch(&m).await?,
    };
    let (data, too_big) = out.unwrap_or_default();
    Ok(respond(data, too_big, a.repo.visibility))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn suffixes() {
        assert_eq!(split_suffix("12.diff"), ("12", Some(Format::Diff)));
        assert_eq!(split_suffix("abc.patch"), ("abc", Some(Format::Patch)));
        assert_eq!(split_suffix("12"), ("12", None));
        assert_eq!(split_suffix("x.diff.txt"), ("x.diff.txt", None));
    }
}
