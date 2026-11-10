// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Which tool's commands the UI shows (D-44): Git and jj get the same
//! pages; only command blocks (checkout, resolving conflicts, catching up
//! after a server-side change) differ. A user's setting decides; on
//! "automatic", their recent pushes do: jj writes a `change-id` header
//! into every commit it makes, Git doesn't. Pushes the server makes
//! itself (merges, updates, suggestions) don't count. Unknown means the
//! UI lets the viewer choose, with nothing preselected.

use slath_core::authz::Subject;
use slath_core::ids::{RepoId, UserId};

use crate::Services;
use crate::error::{Result, ServiceError};

/// A version control tool.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tool {
    Git,
    Jj,
}

impl Tool {
    fn parse(s: &str) -> Option<Self> {
        match s {
            "git" => Some(Self::Git),
            "jj" => Some(Self::Jj),
            _ => None,
        }
    }

    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Git => "git",
            Self::Jj => "jj",
        }
    }
}

/// A user's tool: the setting (`None`: automatic) and what their pushes
/// came from.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ToolPref {
    pub setting: Option<Tool>,
    pub detected: Option<Tool>,
}

impl ToolPref {
    /// The tool whose commands to show, if known.
    #[must_use]
    pub fn effective(&self) -> Option<Tool> {
        self.setting.or(self.detected)
    }
}

/// How many of a push's newest commits are looked at.
const SAMPLE: usize = 20;

impl Services {
    /// The viewer's tool preference (nothing for visitors).
    pub async fn tool_pref(&self, subject: &Subject) -> Result<ToolPref> {
        let Subject::User { id, .. } = subject else {
            return Ok(ToolPref::default());
        };
        let (setting, detected) = slath_db::users::vcs_tools(&self.pool, *id).await?;
        Ok(ToolPref {
            setting: setting.as_deref().and_then(Tool::parse),
            detected: detected.as_deref().and_then(Tool::parse),
        })
    }

    /// Sets the viewer's tool: `auto`, `git`, or `jj`.
    pub async fn set_vcs_tool(&self, subject: &Subject, value: &str) -> Result<()> {
        let user = crate::tokens::user_with_scope(subject, slath_core::authz::Scope::UserWrite)?;
        let tool = match value {
            "auto" | "" => None,
            v => Some(Tool::parse(v).ok_or_else(|| {
                ServiceError::invalid("tool", "invalid", "Choose automatic, Git, or jj.")
            })?),
        };
        slath_db::users::set_vcs_tool(&self.pool, user, tool.map(Tool::as_str)).await?;
        Ok(())
    }

    /// After a push from a client (`ref.updated`): which tool made its new
    /// commits, remembered for the pusher.
    pub(crate) async fn note_push_tool(
        &self,
        repo: RepoId,
        push: uuid::Uuid,
        pusher: Option<UserId>,
        updates: &[slath_git::ipc::RefUpdate],
    ) -> Result<()> {
        let Some(user) = pusher else {
            return Ok(());
        };
        if !matches!(
            slath_db::refs::push_transport(&self.pool, push)
                .await?
                .as_deref(),
            Some("http" | "ssh")
        ) {
            return Ok(());
        }
        let dir = self.repo_path(repo);
        let default = match slath_db::repos::get(&self.pool, repo).await? {
            Some(r) => {
                let name = slath_git::args::RefName::branch(r.default_branch.as_bytes())
                    .map_err(ServiceError::internal)?;
                slath_git::refs::read(&self.runner, &dir, &name)
                    .await
                    .map_err(ServiceError::internal)?
            }
            None => return Ok(()),
        };
        let zero = |s: &str| s.bytes().all(|b| b == b'0');
        let (mut jj, mut total) = (0, 0);
        let mut reader = slath_git::ObjectReader::open(&self.runner, &dir)
            .await
            .map_err(ServiceError::internal)?;
        for u in updates {
            if !u.name.starts_with(b"refs/heads/") || zero(&u.new) {
                continue;
            }
            let new =
                slath_git::ObjectId::parse(u.new.as_bytes()).map_err(ServiceError::internal)?;
            // The commits the push brought: since the old tip, or for a new
            // branch since where it left the default branch.
            let base = if zero(&u.old) {
                match &default {
                    Some(d) if *d != new => {
                        slath_git::browse::merge_base(&self.runner, &dir, d, &new)
                            .await
                            .map_err(ServiceError::internal)?
                    }
                    _ => None,
                }
            } else {
                Some(slath_git::ObjectId::parse(u.old.as_bytes()).map_err(ServiceError::internal)?)
            };
            let commits =
                slath_git::browse::log_range(&self.runner, &dir, base.as_ref(), &new, SAMPLE)
                    .await
                    .map_err(ServiceError::internal)?;
            for c in commits {
                let raw = reader
                    .read_raw_commit(&c.oid)
                    .await
                    .map_err(ServiceError::internal)?;
                total += 1;
                if raw.header(b"change-id").is_some() {
                    jj += 1;
                }
            }
        }
        if total > 0 {
            let tool = if jj * 2 >= total { Tool::Jj } else { Tool::Git };
            slath_db::users::set_pushed_tool(&self.pool, user, tool.as_str()).await?;
        }
        Ok(())
    }
}
