// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Change sources: branches under the repository's prefix (`push-` by
//! default, as `jj git push --change` names its bookmarks) that carry a
//! merge request and nothing else. Branch lists leave them out.

use slath_core::authz::{Action, Subject};
use slath_core::change_source::check_prefix;
pub use slath_core::change_source::is_source;

use crate::Services;
use crate::error::{Result, ServiceError};
use crate::repos::RepoAccess;

impl Services {
    /// Sets the prefix of the repository's change sources (administrators);
    /// empty turns them off.
    pub async fn set_change_source_prefix(
        &self,
        subject: &Subject,
        a: &RepoAccess,
        prefix: &str,
    ) -> Result<()> {
        self.authorize(subject, a, Action::RepoSettings)?;
        let prefix = prefix.trim();
        check_prefix(prefix)
            .map_err(|m| ServiceError::invalid("change_source_prefix", "invalid", m))?;
        if is_source(prefix, &a.repo.default_branch) {
            return Err(ServiceError::invalid(
                "change_source_prefix",
                "default_branch",
                "The default branch starts with this prefix; choose another.",
            ));
        }
        if prefix == a.repo.change_source_prefix {
            return Ok(());
        }
        slath_db::repos::set_change_source_prefix(&self.pool, a.repo.id, prefix).await?;
        self.audit(
            subject,
            "repo.change_source_prefix",
            crate::audit::Target::repo(*a.repo.id.as_uuid()),
            serde_json::json!({ "prefix": prefix }),
        )
        .await;
        Ok(())
    }
}
