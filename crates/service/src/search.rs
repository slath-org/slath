// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Search over issues (with their comments) and repositories.
//!
//! What the subject may read is part of the database query, before ranking
//! and paging, so a private match is never counted, ranked, or returned,
//! and pages stay full.

use slath_core::authz::{Action, Credential, Scope, Subject, UserState};
use slath_db::search::{IssueHit, RepoHit, Viewer};

use crate::Services;
use crate::error::Result;

/// Longest query in characters.
const MAX_QUERY: usize = 200;

fn viewer(subject: &Subject) -> Viewer {
    match subject {
        Subject::User {
            id,
            state: UserState::Active,
            is_site_admin,
            credential,
            ..
        } => match credential {
            // Without `repo:read`, a token searches what anonymous visitors see.
            Credential::Token { scopes, .. }
                if !scopes.iter().any(|s| s.implies(Scope::RepoRead)) =>
            {
                Viewer::default()
            }
            Credential::Token {
                repo_ids: Some(ids),
                ..
            } => Viewer {
                user: Some(*id),
                site_admin: false,
                only: Some(ids.clone()),
            },
            _ => Viewer {
                user: Some(*id),
                site_admin: *is_site_admin,
                only: None,
            },
        },
        _ => Viewer::default(),
    }
}

fn clean(q: &str) -> String {
    q.trim()
        .chars()
        .filter(|c| !c.is_control())
        .take(MAX_QUERY)
        .collect()
}

impl Services {
    /// Issues matching `q` that the subject can read, best first. With
    /// `repo`, only that repository (which must be readable).
    pub async fn search_issues(
        &self,
        subject: &Subject,
        q: &str,
        repo: Option<(&str, &str)>,
        offset: i64,
        limit: i64,
    ) -> Result<Vec<IssueHit>> {
        let q = clean(q);
        let repo_id = match repo {
            Some((ns, name)) => Some(
                self.repo_access(subject, ns, name, Action::RepoRead)
                    .await?
                    .repo
                    .id,
            ),
            None => None,
        };
        if q.is_empty() {
            return Ok(Vec::new());
        }
        Ok(slath_db::search::issues(
            &self.pool,
            &viewer(subject),
            &q,
            repo_id,
            offset.max(0),
            limit.clamp(1, 201),
        )
        .await?)
    }

    /// Repositories whose name or description contains `q`.
    pub async fn search_repos(
        &self,
        subject: &Subject,
        q: &str,
        offset: i64,
        limit: i64,
    ) -> Result<Vec<RepoHit>> {
        let q = clean(q);
        if q.is_empty() {
            return Ok(Vec::new());
        }
        Ok(slath_db::search::repos(
            &self.pool,
            &viewer(subject),
            &q,
            offset.max(0),
            limit.clamp(1, 201),
        )
        .await?)
    }
}
