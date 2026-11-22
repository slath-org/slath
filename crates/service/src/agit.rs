// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Git pushes to `refs/for/<target>[/<topic>]` (the AGit flow): a merge
//! request without naming a branch. Pre-receive maps each such command to
//! a change source of the repository and checks it like a push of that
//! branch; Git hands the commands to the proc-receive hook, which has the
//! server write the change source; post-receive then sees an ordinary
//! branch update and opens or updates the merge request.

use slath_core::change_identity::{AliasKind, trailer_alias};
use slath_git::args::RefName;
use slath_git::ipc::{ProcResult, Quarantine, RefUpdate};

use crate::Services;
use crate::push::PushGrant;
use crate::repos::RepoAccess;

/// The prefix of AGit commands.
pub const PREFIX: &[u8] = b"refs/for/";
/// The longest topic, in bytes.
const MAX_TOPIC: usize = 64;
/// The most path components a `refs/for/` name may have.
const MAX_PARTS: usize = 16;

/// A push to `refs/for/…`, mapped to the change source it updates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgitUpdate {
    /// The command's ref as pushed.
    pub pseudo: Vec<u8>,
    /// The change source's update: `refs/heads/<prefix><rest>`, its value
    /// when pre-receive checked it (zero: new), and the pushed commit.
    pub update: RefUpdate,
    /// The branch the merge request goes into.
    pub target: String,
    /// The name after the prefix: the topic, or the tip's change or
    /// commit ID.
    pub rest: String,
    /// `rest` is the tip's change ID, so later versions map to it too.
    pub by_change: bool,
}

impl AgitUpdate {
    /// What to push to update the merge request again.
    #[must_use]
    pub fn again(&self) -> String {
        if self.by_change {
            format!("refs/for/{}", self.target)
        } else {
            format!("refs/for/{}/{}", self.target, self.rest)
        }
    }
}

const ZERO: &str = "0000000000000000000000000000000000000000";

impl Services {
    /// Maps one `refs/for/…` command to its change source, or says why it
    /// can't be pushed. Reads the tip from the push's quarantine.
    pub(crate) async fn map_agit(
        &self,
        a: &RepoAccess,
        u: &RefUpdate,
        quarantine: &Quarantine,
    ) -> Result<AgitUpdate, String> {
        let prefix = &a.repo.change_source_prefix;
        if prefix.is_empty() {
            return Err("pushes to refs/for/ are turned off in this repository".to_owned());
        }
        if u.new.bytes().all(|b| b == b'0') {
            return Err("refs/for/ can't be deleted".to_owned());
        }
        let rest = u
            .name
            .strip_prefix(PREFIX)
            .and_then(|r| std::str::from_utf8(r).ok())
            .ok_or("not a valid name")?;
        let parts: Vec<&str> = rest.split('/').collect();
        if parts.len() > MAX_PARTS {
            return Err("too many levels".to_owned());
        }
        let dir = self.repo_path(a.repo.id);
        // The longest leading part that is a branch is the target.
        let mut found = None;
        for k in (1..=parts.len()).rev() {
            let target = parts[..k].join("/");
            let Ok(r) = RefName::branch(target.as_bytes()) else {
                continue;
            };
            match slath_git::refs::read(&self.runner, &dir, &r).await {
                Ok(Some(_)) => {
                    found = Some((target, parts[k..].join("/")));
                    break;
                }
                Ok(None) => {}
                Err(e) => {
                    tracing::warn!(error = %e, "reading an AGit target failed");
                    return Err("internal error, try again".to_owned());
                }
            }
        }
        let (target, topic) =
            found.ok_or_else(|| format!("no branch {rest} to open a merge request into"))?;
        if crate::change_sources::is_source(prefix, &target) {
            return Err(format!("{target} is a change source, not a target"));
        }
        let tip = slath_git::ObjectId::parse(&u.new).map_err(|_| "not a valid commit")?;
        let (rest, by_change) = if topic.is_empty() {
            self.tip_name(a, &tip, quarantine).await?
        } else if topic.len() > MAX_TOPIC {
            return Err(format!("use a topic of at most {MAX_TOPIC} bytes"));
        } else {
            (topic, false)
        };
        let branch = format!("{prefix}{rest}");
        let name = RefName::branch(branch.as_bytes())
            .map_err(|_| format!("{branch} isn't a valid branch name"))?;
        let old = match slath_git::refs::read(&self.runner, &dir, &name).await {
            Ok(Some(o)) => o.as_str().to_owned(),
            Ok(None) => ZERO.to_owned(),
            Err(e) => {
                tracing::warn!(error = %e, "reading a change source failed");
                return Err("internal error, try again".to_owned());
            }
        };
        Ok(AgitUpdate {
            pseudo: u.name.clone(),
            update: RefUpdate {
                old,
                new: u.new.clone(),
                name: name.as_bytes().to_vec(),
            },
            target,
            rest,
            by_change,
        })
    }

    /// The name of a tip's change source without a topic: its change ID
    /// (jj's header, else a `Change-Id` trailer), else its commit ID.
    async fn tip_name(
        &self,
        a: &RepoAccess,
        tip: &slath_git::ObjectId,
        quarantine: &Quarantine,
    ) -> Result<(String, bool), String> {
        let dir = self.repo_path(a.repo.id);
        let q = quarantine
            .object_directory
            .as_deref()
            .map(std::path::Path::new);
        let commit = match slath_git::ObjectReader::open_in(&self.runner, &dir, q).await {
            Ok(mut r) => r.read_raw_commit(tip).await,
            Err(e) => Err(e),
        };
        let commit = commit.map_err(|e| {
            tracing::warn!(error = %e, "reading an AGit tip failed");
            "the pushed commit can't be read".to_owned()
        })?;
        if let Some(id) = commit.change_id() {
            return Ok((id[..12].to_owned(), true));
        }
        if let Some(alias) = slath_git::changes::change_id_trailer(&commit.message)
            .as_deref()
            .and_then(trailer_alias)
        {
            let n = if alias.kind == AliasKind::Gerrit {
                13
            } else {
                12
            };
            return Ok((alias.value[..n].to_owned(), true));
        }
        Ok((tip.as_str()[..12].to_owned(), false))
    }

    /// proc-receive: writes the change source of each command that
    /// pre-receive mapped, against the value it checked. Every command
    /// gets a result.
    pub async fn proc_receive(
        &self,
        grant: &PushGrant,
        agit: &[AgitUpdate],
        updates: &[RefUpdate],
    ) -> Vec<ProcResult> {
        let dir = self.repo_path(grant.repo_id);
        let mut out = Vec::new();
        for u in updates {
            let Some(a) = agit
                .iter()
                .find(|a| a.pseudo == u.name && a.update.new == u.new)
            else {
                out.push(ProcResult::refuse(&u.name, "not checked"));
                continue;
            };
            let parse = |s: &str| slath_git::ObjectId::parse(s).ok();
            let (Ok(name), Some(new)) =
                (RefName::parse(a.update.name.clone()), parse(&a.update.new))
            else {
                out.push(ProcResult::refuse(&u.name, "not a valid update"));
                continue;
            };
            let old = parse(&a.update.old).filter(|o| o.as_str() != ZERO);
            if let Err(e) =
                slath_git::refs::update_cas(&self.runner, &dir, &name, Some(&new), old.as_ref())
                    .await
            {
                tracing::info!(error = %e, "an AGit push lost a race");
                out.push(ProcResult::refuse(&u.name, "changed meanwhile, push again"));
                continue;
            }
            let forced = match &old {
                Some(o) => !slath_git::storage::is_ancestor(&self.runner, &dir, o, &new, None)
                    .await
                    .unwrap_or(false),
                None => false,
            };
            out.push(ProcResult {
                name: u.name.clone(),
                refused: None,
                refname: a.update.name.clone(),
                old: a.update.old.clone(),
                new: a.update.new.clone(),
                forced,
            });
        }
        out
    }
}
