// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! The push pipeline: authorization of Git transport requests, the
//! pre-receive policy, and the post-receive bookkeeping.

use slath_core::authz::{Action, Credential, RepoState, Subject, UserState};
use slath_core::ids::{RepoId, SshKeyId, UserId};
use slath_git::args::RefName;
use slath_git::ipc::{Quarantine, RefUpdate};

use crate::Services;
use crate::error::{Result, ServiceError};
use crate::repos::{RepoAccess, Resolved};

/// How a push arrived.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transport {
    Http,
    Ssh,
    Internal,
}

impl Transport {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Http => "http",
            Self::Ssh => "ssh",
            Self::Internal => "internal",
        }
    }
}

/// What a hook token stands for: who pushes, where, and how. The
/// pre-receive policy re-checks with exactly this subject (including a
/// token's scopes and repository restriction).
#[derive(Debug, Clone)]
pub struct PushGrant {
    pub subject: Subject,
    pub repo_id: RepoId,
    pub transport: Transport,
}

impl PushGrant {
    fn user_id(&self) -> Option<UserId> {
        match &self.subject {
            Subject::User { id, .. } => Some(*id),
            _ => None,
        }
    }
}

/// Why a ref update was refused (also the metric label).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DenyReason {
    ReservedRef,
    NotBranchOrTag,
    InvalidRef,
    NoPermission,
    Archived,
    TagMove,
}

impl DenyReason {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ReservedRef => "reserved_ref",
            Self::NotBranchOrTag => "not_branch_or_tag",
            Self::InvalidRef => "invalid_ref",
            Self::NoPermission => "no_permission",
            Self::Archived => "archived",
            Self::TagMove => "tag_move",
        }
    }
}

/// The pre-receive verdict: the push is accepted only if `denied` is empty.
#[derive(Debug, Clone, Default)]
pub struct Verdict {
    pub messages: Vec<String>,
    pub denied: Vec<DenyReason>,
}

fn is_zero(oid: &str) -> bool {
    oid.bytes().all(|b| b == b'0')
}

/// An SSH key that may authenticate, with its owner.
#[derive(Debug, Clone)]
pub struct SshKeyGrant {
    pub key_id: SshKeyId,
    pub key_type: String,
    pub key_base64: String,
}

impl Services {
    /// `AuthorizedKeysCommand`: the key with this exact fingerprint, if its
    /// owner can use it.
    pub async fn ssh_lookup(&self, fingerprint: &str) -> Result<Option<SshKeyGrant>> {
        if !slath_auth::ssh_key::is_fingerprint(fingerprint) {
            return Ok(None);
        }
        let Some(k) = slath_db::ssh_keys::find_by_fingerprint(&self.pool, fingerprint).await?
        else {
            return Ok(None);
        };
        if k.user_state != UserState::Active {
            return Ok(None);
        }
        Ok(Some(SshKeyGrant {
            key_id: k.key.id,
            key_type: k.key.key_type,
            key_base64: k.key.public_key,
        }))
    }

    /// The subject an SSH key authenticates as (`None` for unknown or
    /// unusable keys). Records the key's use.
    pub async fn ssh_subject(&self, key_id: SshKeyId) -> Result<Option<Subject>> {
        let Some(k) = slath_db::ssh_keys::find_by_id(&self.pool, key_id).await? else {
            return Ok(None);
        };
        if k.user_state != UserState::Active {
            return Ok(None);
        }
        slath_db::ssh_keys::touch(&self.pool, key_id).await?;
        let is_site_admin = k.is_admin && self.admin_mfa_satisfied(k.key.user_id, None).await?;
        Ok(Some(Subject::User {
            id: k.key.user_id,
            kind: k.user_kind,
            state: k.user_state,
            is_site_admin,
            credential: Credential::SshKey { id: key_id },
        }))
    }

    /// Authorizes a Git transport request: reading needs `repo.read`,
    /// pushing `repo.push`. Renamed repositories are followed for subjects
    /// who can read them. Everything else is "not found".
    pub async fn transport_access(
        &self,
        subject: &Subject,
        namespace: &str,
        name: &str,
        write: bool,
    ) -> Result<RepoAccess> {
        let action = if write {
            Action::RepoPush
        } else {
            Action::RepoRead
        };
        let (ns, name) = match self.resolve_repo(subject, namespace, name).await? {
            Resolved::Moved { namespace, name } => (namespace, name),
            Resolved::Repo(a) => (a.repo.namespace, a.repo.name),
        };
        self.repo_access(subject, &ns, &name, action).await
    }

    /// The pre-receive policy, for all updates of one push (all or nothing):
    /// reserved refs, branches and tags only, valid names, push permission,
    /// and the maintain role for moving or deleting tags.
    pub async fn pre_receive(
        &self,
        grant: &PushGrant,
        updates: &[RefUpdate],
        _quarantine: &Quarantine,
    ) -> Verdict {
        let mut v = Verdict::default();
        // Permissions are re-checked now, with the push's own credential.
        let access = match self
            .repo_access_by_id(&grant.subject, grant.repo_id, Action::RepoRead)
            .await
        {
            Ok(a) => Some(a),
            Err(ServiceError::NotFound | ServiceError::Forbidden) => None,
            Err(e) => {
                tracing::error!(error = %e, "pre-receive: loading the repository failed");
                v.denied.push(DenyReason::NoPermission);
                v.messages
                    .push("slath: push rejected: internal error, try again".to_owned());
                return v;
            }
        };
        let can = |action| {
            access
                .as_ref()
                .is_some_and(|a| a.can(&grant.subject, action))
        };
        let archived = access
            .as_ref()
            .is_some_and(|a| a.repo.state == RepoState::Archived);
        for u in updates {
            let shown = String::from_utf8_lossy(&u.name).into_owned();
            let mut deny = |reason: DenyReason, msg: &str| {
                v.denied.push(reason);
                v.messages.push(format!("slath: {shown}: {msg}"));
            };
            let r = match RefName::parse(u.name.clone()) {
                Ok(r) => r,
                Err(why) => {
                    deny(DenyReason::InvalidRef, why);
                    continue;
                }
            };
            if r.is_reserved() {
                deny(
                    DenyReason::ReservedRef,
                    "refs/slath/ and refs/merge-requests/ are reserved",
                );
                continue;
            }
            let bytes = r.as_bytes();
            let is_tag = bytes.starts_with(b"refs/tags/");
            if !bytes.starts_with(b"refs/heads/") && !is_tag {
                deny(
                    DenyReason::NotBranchOrTag,
                    "only branches (refs/heads/) and tags (refs/tags/) can be pushed",
                );
                continue;
            }
            if archived {
                deny(DenyReason::Archived, "the repository is archived");
                continue;
            }
            if !can(Action::RepoPush) {
                deny(
                    DenyReason::NoPermission,
                    "you don't have permission to push to this repository",
                );
                continue;
            }
            if is_tag && !is_zero(&u.old) && !can(Action::TagMoveOrDelete) {
                deny(
                    DenyReason::TagMove,
                    "moving or deleting a tag needs the maintain role",
                );
            }
        }
        if !v.denied.is_empty() {
            v.messages.push("slath: push rejected".to_owned());
        }
        v
    }

    /// Records a completed push in one transaction: the push event, its ref
    /// updates, the ref snapshot, and the `ref.updated` job. Never fails
    /// the push; anything missed here is repaired by the reconciler.
    pub async fn post_receive(&self, grant: &PushGrant, updates: &[RefUpdate]) -> Vec<String> {
        let mut messages = Vec::new();
        if let Err(e) = self
            .record_push(
                grant.repo_id,
                grant.user_id(),
                grant.transport.as_str(),
                updates,
            )
            .await
        {
            tracing::error!(error = %e, repo = %grant.repo_id, "post-receive bookkeeping failed; the reconciler will repair it");
        }
        match self.measure_repo(grant.repo_id).await {
            Ok(size) => {
                let quota = self.config.limits.repo_soft_quota_bytes.get();
                if quota > 0 && size > quota {
                    messages.push(format!(
                        "slath: warning: this repository uses {}, more than its soft quota of {}",
                        human_bytes(size),
                        human_bytes(quota)
                    ));
                }
            }
            Err(e) => {
                tracing::warn!(error = %e, repo = %grant.repo_id, "measuring the repository failed")
            }
        }
        messages
    }

    /// Updates a repository's recorded size from disk.
    async fn measure_repo(&self, id: RepoId) -> Result<u64> {
        let size = slath_git::storage::disk_usage(&self.runner, &self.repo_path(id))
            .await
            .map_err(ServiceError::internal)?;
        slath_db::repos::set_size(&self.pool, id, i64::try_from(size).unwrap_or(i64::MAX)).await?;
        Ok(size)
    }

    /// Writes a push event (also used by the reconciler with transport `reconcile`).
    pub async fn record_push(
        &self,
        repo: RepoId,
        pusher: Option<UserId>,
        transport: &str,
        updates: &[RefUpdate],
    ) -> Result<()> {
        let mut tx = self.pool.begin().await.map_err(ServiceError::internal)?;
        let push_id =
            slath_db::refs::record_push(&mut tx, repo, pusher, transport, updates).await?;
        slath_db::jobs::enqueue(
            &mut *tx,
            &slath_db::jobs::NewJob::new(
                "default",
                crate::jobs::REF_UPDATED,
                serde_json::json!({"push_id": push_id.to_string(), "repo_id": repo.to_string()}),
            )
            .dedupe_key(format!("ref.updated:{push_id}")),
        )
        .await?;
        let (pushes, runs) = slath_db::repos::count_push_for_maintenance(&mut *tx, repo).await?;
        if pushes >= crate::jobs::MAINTENANCE_PUSHES {
            slath_db::jobs::enqueue(&mut *tx, &Self::maintenance_job(repo, runs)).await?;
        }
        tx.commit().await.map_err(ServiceError::internal)?;
        Ok(())
    }
}

/// `1536` → `1.5 KiB`.
fn human_bytes(n: u64) -> String {
    const UNITS: [&str; 5] = ["bytes", "KiB", "MiB", "GiB", "TiB"];
    let mut value = n as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{n} bytes")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

#[cfg(test)]
mod size_tests {
    #[test]
    fn human_bytes_picks_a_unit() {
        assert_eq!(super::human_bytes(512), "512 bytes");
        assert_eq!(super::human_bytes(1536), "1.5 KiB");
        assert_eq!(super::human_bytes(10 * 1024 * 1024), "10.0 MiB");
    }
}
