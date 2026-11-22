// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Saved views of a merge request list: a signed-in person's named query
//! strings per repository. A saved view is only a link: opening it lists
//! what its owner can read then, never more.

use slath_core::authz::{Action, Subject};
use slath_core::ids::UserId;
use slath_db::mr_saved_views as db;
pub use slath_db::mr_saved_views::SavedView;

use crate::Services;
use crate::error::{Result, ServiceError};
use crate::repos::RepoAccess;

/// The longest name of a saved view, in characters.
pub const NAME_MAX: usize = 60;
/// The longest query string of a saved view, in bytes.
const QUERY_MAX: usize = 1000;

fn user(subject: &Subject) -> Result<UserId> {
    match subject {
        Subject::User { id, .. } => Ok(*id),
        _ => Err(ServiceError::Unauthenticated),
    }
}

/// Whether `q` is a query string as the merge request list writes it:
/// percent-encoded `key=value` pairs, nothing that could leave the list's
/// own URL.
#[must_use]
pub fn is_list_query(q: &str) -> bool {
    !q.is_empty()
        && q.len() <= QUERY_MAX
        && q.starts_with("state=")
        && q.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"&=%-_.~".contains(&b))
}

impl Services {
    /// The subject's saved views of this repository's merge request list;
    /// none when signed out.
    pub async fn mr_saved_views(
        &self,
        subject: &Subject,
        a: &RepoAccess,
    ) -> Result<Vec<SavedView>> {
        match subject {
            Subject::User { id, .. } => Ok(db::list(&self.pool, *id, a.repo.id).await?),
            _ => Ok(Vec::new()),
        }
    }

    /// Saves the list's `query` under `name` for the subject, replacing
    /// their view of the same name; at most `limits.saved_views_per_repo`
    /// per repository.
    pub async fn save_mr_view(
        &self,
        subject: &Subject,
        ns: &str,
        repo: &str,
        name: &str,
        query: &str,
    ) -> Result<SavedView> {
        let me = user(subject)?;
        let a = self.mrs_access(subject, ns, repo, Action::RepoRead).await?;
        let name = name.trim();
        if name.is_empty() {
            return Err(ServiceError::invalid("name", "required", "Name the view."));
        }
        if name.chars().count() > NAME_MAX || name.chars().any(char::is_control) {
            return Err(ServiceError::invalid(
                "name",
                "invalid",
                format!("Use at most {NAME_MAX} characters, without line breaks."),
            ));
        }
        if !is_list_query(query) {
            return Err(ServiceError::invalid(
                "query",
                "invalid",
                "This isn't a view of the merge request list.",
            ));
        }
        let most = self.config.limits.saved_views_per_repo;
        db::save(
            &self.pool,
            me,
            a.repo.id,
            name,
            query,
            i64::try_from(most).unwrap_or(i64::MAX),
        )
        .await?
        .ok_or_else(|| {
            ServiceError::invalid(
                "name",
                "limit",
                format!(
                    "You can save at most {most} views here (limits.saved_views_per_repo). Delete one first."
                ),
            )
        })
    }

    /// Deletes one of the subject's saved views of this repository.
    pub async fn delete_mr_view(
        &self,
        subject: &Subject,
        ns: &str,
        repo: &str,
        id: uuid::Uuid,
    ) -> Result<()> {
        let me = user(subject)?;
        let a = self.mrs_access(subject, ns, repo, Action::RepoRead).await?;
        if db::delete(&self.pool, me, a.repo.id, id).await? {
            Ok(())
        } else {
            Err(ServiceError::NotFound)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::is_list_query;

    #[test]
    fn only_the_lists_own_query_strings_are_saved() {
        assert!(is_list_query(
            "state=open&labels=bug%2Cui&view=mine&sort=updated"
        ));
        for q in [
            "",
            "labels=bug",
            "state=open&next=//evil.example",
            "state=open\"><script>",
            "state=open labels",
        ] {
            assert!(!is_list_query(q), "{q}");
        }
    }
}
