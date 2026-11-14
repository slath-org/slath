// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Origin labels: the origin a subject's text gets on a repository, as they
//! stand now. Every write of user-generated text records it; edits by
//! someone else keep the less trusted of the two (in SQL).

use slath_core::authz::{Subject, UserKind};
use slath_core::origin::Origin;
use slath_db::repos::Repo;

use crate::Services;
use crate::error::Result;

impl Services {
    /// The origin of text `subject` writes on `repo`: `bot` for any bot,
    /// otherwise from the person's effective role and organization
    /// membership. Slath itself writes as a writer.
    pub(crate) async fn origin_of(&self, subject: &Subject, repo: &Repo) -> Result<Origin> {
        match subject {
            Subject::User {
                kind: UserKind::Bot,
                ..
            } => Ok(Origin::Bot),
            Subject::User { id, .. } => {
                let role = self.effective_role(subject, repo).await?;
                let org_member = role.is_none()
                    && slath_db::repos::role_facts(&self.pool, repo.id, *id)
                        .await?
                        .org_role
                        .is_some();
                Ok(Origin::of_person(role, org_member))
            }
            Subject::System => Ok(Origin::Writer),
            Subject::Anonymous | Subject::DeployKey { .. } => Ok(Origin::Anonymous),
        }
    }
}
