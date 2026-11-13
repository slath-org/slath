// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Sensitive paths: the built-in ones plus a repository's own globs. Bots
//! can't push commits that change them, nor apply suggestions to them.

use slath_core::authz::{Action, Subject, UserKind};
use slath_core::ids::RepoId;
use slath_core::sensitive_paths::{MAX_EXTRA, SensitivePaths, invalid_glob};
use slath_git::ipc::{Quarantine, RefUpdate};

use crate::Services;
use crate::error::{Result, ServiceError};
use crate::push::{DenyReason, PushGrant, Verdict};

/// Paths a rejection names before it only counts the rest.
const SHOWN: usize = 10;

/// A sensitive path a push changes, and the first new commit that does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Touched {
    pub path: String,
    pub commit: String,
}

impl Services {
    /// A repository's sensitive paths: the defaults and its own globs.
    pub async fn sensitive_paths(&self, repo: RepoId) -> Result<SensitivePaths> {
        Ok(SensitivePaths::new(
            slath_db::repos::sensitive_paths(&self.pool, repo).await?,
        ))
    }

    /// A repository's sensitive paths, for whoever can read it.
    pub async fn repo_sensitive_paths(
        &self,
        subject: &Subject,
        ns: &str,
        repo: &str,
    ) -> Result<SensitivePaths> {
        let a = self
            .repo_access(subject, ns, repo, Action::RepoRead)
            .await?;
        self.sensitive_paths(a.repo.id).await
    }

    /// Replaces a repository's own globs (whoever manages its branch
    /// protection); audited. Blank entries are dropped, duplicates kept
    /// once.
    pub async fn set_sensitive_paths(
        &self,
        subject: &Subject,
        ns: &str,
        repo: &str,
        globs: &[String],
    ) -> Result<SensitivePaths> {
        let a = self
            .repo_access(subject, ns, repo, Action::ProtectionManage)
            .await?;
        let mut list: Vec<String> = Vec::new();
        for g in globs.iter().map(|g| g.trim()).filter(|g| !g.is_empty()) {
            if let Some(why) = invalid_glob(g) {
                return Err(ServiceError::invalid(
                    "paths",
                    "invalid",
                    format!("{g} {why}."),
                ));
            }
            if !list.iter().any(|x| x == g) {
                list.push(g.to_owned());
            }
        }
        if list.len() > MAX_EXTRA {
            return Err(ServiceError::invalid(
                "paths",
                "too_many",
                format!("Add at most {MAX_EXTRA} paths."),
            ));
        }
        slath_db::repos::set_sensitive_paths(&self.pool, a.repo.id, &list).await?;
        self.audit(
            subject,
            "repo.sensitive_paths",
            crate::audit::Target::repo(*a.repo.id.as_uuid()),
            serde_json::json!({"paths": list}),
        )
        .await;
        Ok(SensitivePaths::new(list))
    }

    /// For a bot's push: rejects it if a commit it adds changes a
    /// sensitive path. Fails closed: a check that can't finish rejects.
    pub(crate) async fn check_sensitive_paths(
        &self,
        grant: &PushGrant,
        updates: &[RefUpdate],
        quarantine: &Quarantine,
        v: &mut Verdict,
    ) {
        if !matches!(
            grant.subject,
            Subject::User {
                kind: UserKind::Bot,
                ..
            }
        ) {
            return;
        }
        let timeout = self.config.limits.secret_scan_timeout.get();
        let touched = match tokio::time::timeout(
            timeout,
            self.touched_sensitive_paths(grant, updates, quarantine),
        )
        .await
        {
            Ok(Ok(t)) => t,
            Ok(Err(e)) => {
                tracing::error!(error = %e, "pre-receive: checking sensitive paths failed");
                v.denied.push(DenyReason::SensitivePath);
                v.messages.push(
                    "slath: push rejected: sensitive paths couldn't be checked; try again"
                        .to_owned(),
                );
                return;
            }
            Err(_) => {
                v.denied.push(DenyReason::SensitivePath);
                v.messages.push(format!(
                    "slath: push rejected: checking sensitive paths took longer than limits.secret_scan_timeout ({} s)",
                    timeout.as_secs()
                ));
                return;
            }
        };
        if touched.is_empty() {
            return;
        }
        v.denied.push(DenyReason::SensitivePath);
        v.messages
            .push("slath: bots can't change sensitive paths:".to_owned());
        for t in touched.iter().take(SHOWN) {
            let short = t.commit.get(..10).unwrap_or(&t.commit);
            v.messages
                .push(format!("slath:   {} (commit {short})", t.path));
        }
        if touched.len() > SHOWN {
            v.messages
                .push(format!("slath:   and {} more", touched.len() - SHOWN));
        }
        v.messages
            .push("slath: ask a person to make this change".to_owned());
        self.audit(
            &grant.subject,
            "push.sensitive_rejected",
            crate::audit::Target::repo(*grant.repo_id.as_uuid()),
            serde_json::json!({
                "transport": grant.transport.as_str(),
                "paths": touched.iter().map(|t| &t.path).collect::<Vec<_>>(),
                "commits": touched.iter().map(|t| &t.commit).collect::<Vec<_>>(),
            }),
        )
        .await;
    }

    /// Each sensitive path the commits of a push change, once, with the
    /// oldest new commit that changes it.
    async fn touched_sensitive_paths(
        &self,
        grant: &PushGrant,
        updates: &[RefUpdate],
        quarantine: &Quarantine,
    ) -> Result<Vec<Touched>> {
        let tips: Vec<slath_git::ObjectId> = updates
            .iter()
            .filter(|u| !u.new.bytes().all(|b| b == b'0'))
            .filter_map(|u| slath_git::ObjectId::parse(&u.new).ok())
            .collect();
        let q = quarantine
            .object_directory
            .as_deref()
            .map(std::path::Path::new);
        let changed =
            slath_git::scan::changed_paths(&self.runner, &self.repo_path(grant.repo_id), q, &tips)
                .await
                .map_err(ServiceError::internal)?;
        let sensitive = self.sensitive_paths(grant.repo_id).await?;
        let mut out: Vec<Touched> = Vec::new();
        for (commit, paths) in changed {
            for p in paths {
                if sensitive.matches(&p) {
                    let path = String::from_utf8_lossy(&p).into_owned();
                    if !out.iter().any(|t| t.path == path) {
                        out.push(Touched {
                            path,
                            commit: commit.to_string(),
                        });
                    }
                }
            }
        }
        Ok(out)
    }
}
