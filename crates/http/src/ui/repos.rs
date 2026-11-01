// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Repository pages.

use slath_core::authz::Subject;

use crate::AppState;
use crate::error::AppError;

/// A repository in a list (profile pages, explore).
#[derive(Debug, Clone)]
pub struct RepoCard {
    pub namespace: String,
    pub name: String,
    pub description: Option<String>,
    pub private: bool,
    pub archived: bool,
}

/// The repositories of a namespace the subject can see.
pub async fn cards_for_namespace(
    _state: &AppState,
    _subject: &Subject,
    _ns: &str,
) -> Result<Vec<RepoCard>, AppError> {
    Ok(Vec::new())
}
