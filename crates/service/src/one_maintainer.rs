// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! The one-maintainer policy: with the protection setting on and exactly
//! one person with write access, that person's own review of the exact
//! head counts where only another person's approval could otherwise. The
//! fact is computed from the grants on every evaluation, so a second
//! writer ends the policy at once.

use slath_core::authz::Role;
use slath_core::ids::UserId;
use slath_db::repos::Repo;

use crate::Services;
use crate::error::Result;
use crate::protections::Effective;

/// The policy as it stands for one protection and repository.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OneMaintainer {
    /// The only person with write access, when there is exactly one.
    pub maintainer: Option<(UserId, String)>,
    /// The setting is on and the fact holds.
    pub applies: bool,
}

impl Services {
    /// The only active person with write access or higher on the
    /// repository by their grants; `None` with none or several. Bots and
    /// instance admins without a grant don't count.
    pub(crate) async fn sole_maintainer(&self, repo: &Repo) -> Result<Option<UserId>> {
        let mut found = None;
        for u in slath_db::repos::writer_candidates(&self.pool, repo.id).await? {
            if self
                .granted_role(repo, u)
                .await?
                .is_some_and(|r| r >= Role::Write)
            {
                if found.is_some() {
                    return Ok(None);
                }
                found = Some(u);
            }
        }
        Ok(found)
    }

    /// The one-maintainer policy for a protection; `None` without the
    /// setting.
    pub(crate) async fn one_maintainer(
        &self,
        repo: &Repo,
        eff: &Effective,
    ) -> Result<Option<OneMaintainer>> {
        if !eff.single_maintainer {
            return Ok(None);
        }
        let Some(u) = self.sole_maintainer(repo).await? else {
            return Ok(Some(OneMaintainer {
                maintainer: None,
                applies: false,
            }));
        };
        let name = slath_db::users::get(&self.pool, u)
            .await?
            .map(|x| x.name)
            .unwrap_or_default();
        Ok(Some(OneMaintainer {
            maintainer: Some((u, name)),
            applies: true,
        }))
    }
}
