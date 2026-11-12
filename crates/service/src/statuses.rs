// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Commit statuses: reports on a commit by an authenticated reporter
//! (evidence class "reported": Slath knows who reported it, not that the
//! check ran), and which reports satisfy a merge request's required checks
//! (spec/07 commit statuses, spec/05 verdicts and requirements).

use slath_core::authz::{Action, Subject};
use slath_core::ids::UserId;
use slath_db::merge_requests::MergeRequest;
use slath_db::statuses::{NewStatus, Status};
use slath_git::ObjectId;

use crate::Services;
use crate::error::{Result, ServiceError};
use crate::mr_reviews::Requirement;
use crate::protections::{Effective, RequiredCheck};

/// At most this many statuses per commit and context.
pub const MAX_PER_CONTEXT: i64 = 1000;

/// A status as reported.
#[derive(Debug, Clone, Default)]
pub struct StatusInput {
    pub state: String,
    pub context: String,
    pub description: Option<String>,
    pub target_url: Option<String>,
}

fn check_url(u: &str) -> Result<()> {
    let bad = || {
        ServiceError::invalid(
            "target_url",
            "invalid",
            "Use an https:// URL (http:// only for localhost).",
        )
    };
    let url = url::Url::parse(u).map_err(|_| bad())?;
    let local = matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"));
    match url.scheme() {
        "https" => {}
        "http" if local => {}
        _ => return Err(bad()),
    }
    if u.len() > 2048 {
        return Err(bad());
    }
    Ok(())
}

/// The latest status of each context, newest first.
#[must_use]
pub fn latest_per_context(all: &[Status]) -> Vec<Status> {
    let mut out: Vec<Status> = Vec::new();
    for s in all {
        if !out.iter().any(|o| o.context == s.context) {
            out.push(s.clone());
        }
    }
    out
}

impl Services {
    /// Reports a status on a commit of the repository (`status.write`).
    pub async fn report_status(
        &self,
        subject: &Subject,
        (ns, repo): (&str, &str),
        oid: &str,
        s: &StatusInput,
    ) -> Result<Status> {
        let a = self
            .repo_access(subject, ns, repo, Action::StatusWrite)
            .await?;
        let Subject::User { id: reporter, .. } = subject else {
            return Err(ServiceError::Unauthenticated);
        };
        if !matches!(
            s.state.as_str(),
            "pending" | "success" | "failure" | "error"
        ) {
            return Err(ServiceError::invalid(
                "state",
                "invalid",
                "Use pending, success, failure, or error.",
            ));
        }
        let context = s.context.trim();
        if !crate::protections::valid_context(context) {
            return Err(ServiceError::invalid(
                "context",
                "invalid",
                "Use up to 100 letters, digits, spaces, and . _ / : -",
            ));
        }
        let description = s
            .description
            .as_deref()
            .map(str::trim)
            .filter(|d| !d.is_empty());
        if description.is_some_and(|d| d.chars().count() > 1000 || d.chars().any(char::is_control))
        {
            return Err(ServiceError::invalid(
                "description",
                "invalid",
                "Use at most 1000 characters, without control characters.",
            ));
        }
        let target_url = s
            .target_url
            .as_deref()
            .map(str::trim)
            .filter(|u| !u.is_empty());
        if let Some(u) = target_url {
            check_url(u)?;
        }
        // Only full commit IDs of this repository.
        let oid = ObjectId::parse(oid).map_err(|_| ServiceError::NotFound)?;
        let kind = self
            .runner
            .repo(&self.repo_path(a.repo.id))
            .args(["cat-file", "-t", "--end-of-options", oid.as_str()])
            .output()
            .await
            .map_err(ServiceError::internal)?;
        if !kind.status.success() || kind.stdout.trim_ascii() != b"commit" {
            return Err(ServiceError::NotFound);
        }
        if slath_db::statuses::count(&self.pool, a.repo.id, oid.as_str(), context).await?
            >= MAX_PER_CONTEXT
        {
            return Err(ServiceError::invalid(
                "context",
                "too_many",
                format!("This commit already has {MAX_PER_CONTEXT} statuses for this context."),
            ));
        }
        let id = slath_db::statuses::insert(
            &self.pool,
            &NewStatus {
                repo: a.repo.id,
                commit_oid: oid.as_str(),
                context,
                state: &s.state,
                description,
                target_url,
                reporter: *reporter,
            },
        )
        .await?;
        if s.state == "success" {
            self.queue_auto_merges_in(a.repo.id).await;
        }
        self.emit_webhook(
            a.repo.id,
            Some(*reporter),
            ("status", Some(s.state.as_str())),
            crate::webhooks::Needs::Repo,
            serde_json::json!({"status": {
                "commit": oid.as_str(),
                "context": context,
                "state": s.state,
                "description": description,
                "target_url": target_url,
            }}),
        )
        .await;
        slath_db::statuses::for_commit(&self.pool, a.repo.id, oid.as_str())
            .await?
            .into_iter()
            .find(|s| s.id == id)
            .ok_or(ServiceError::NotFound)
    }

    /// The latest status of each context on a commit.
    pub async fn commit_statuses(
        &self,
        subject: &Subject,
        (ns, repo): (&str, &str),
        oid: &str,
    ) -> Result<Vec<Status>> {
        let a = self
            .repo_access(subject, ns, repo, Action::RepoRead)
            .await?;
        let oid = ObjectId::parse(oid).map_err(|_| ServiceError::NotFound)?;
        let all = slath_db::statuses::for_commit(&self.pool, a.repo.id, oid.as_str()).await?;
        Ok(latest_per_context(&all))
    }

    /// The latest status per context of each of a merge request's
    /// commits.
    pub async fn statuses_of_commits(
        &self,
        m: &MergeRequest,
        oids: &[String],
    ) -> Result<std::collections::HashMap<String, Vec<Status>>> {
        let mut out = std::collections::HashMap::new();
        for oid in oids {
            let all = slath_db::statuses::for_commit(&self.pool, m.repo_id, oid).await?;
            if !all.is_empty() {
                out.insert(oid.clone(), latest_per_context(&all));
            }
        }
        Ok(out)
    }

    /// The latest statuses of the merge request's head.
    pub async fn mr_statuses(&self, m: &MergeRequest) -> Result<Vec<Status>> {
        let Some(head) = m.head_oid.as_deref() else {
            return Ok(Vec::new());
        };
        let all = slath_db::statuses::for_commit(&self.pool, m.repo_id, head).await?;
        Ok(latest_per_context(&all))
    }

    /// Why a reporter's status doesn't count for this merge request, if it
    /// doesn't.
    async fn not_counting(
        &self,
        m: &MergeRequest,
        check: &RequiredCheck,
        reporter: Option<UserId>,
    ) -> Result<Option<&'static str>> {
        let Some(u) = reporter else {
            return Ok(Some("its reporter's account was deleted"));
        };
        if m.author_id == Some(u) {
            return Ok(Some("the author can't report their own checks"));
        }
        if m.author_id.is_some()
            && slath_db::users::bot_owner_user(&self.pool, u).await? == m.author_id
        {
            return Ok(Some("bots of the author can't report its checks"));
        }
        if check.reporters.as_ref().is_some_and(|r| !r.contains(&u)) {
            return Ok(Some("only the listed reporters count"));
        }
        Ok(None)
    }

    /// One requirement per required check: its latest status from a
    /// reporter who counts, on the commit that will land (spec/05).
    pub(crate) async fn check_requirements(
        &self,
        m: &MergeRequest,
        eff: &Effective,
        up_to_date: bool,
    ) -> Result<Vec<Requirement>> {
        let mut out = Vec::new();
        if eff.required_checks.is_empty() {
            return Ok(out);
        }
        let Some(head) = m.head_oid.as_deref() else {
            return Ok(out);
        };
        let all = slath_db::statuses::for_commit(&self.pool, m.repo_id, head).await?;
        for check in &eff.required_checks {
            let ctx = &check.context;
            if !eff.statuses_on_head_ok && !up_to_date {
                // Rebase and merge commits are new commits; they need their own.
                out.push(Requirement {
                    key: "statuses",
                    state: "pending",
                    tone: "waiting",
                    reason: format!("{ctx}: needs a status on the commit that lands, since the head's don't count here."),
                    fix: Some("Update the branch so it can be fast-forwarded".to_owned()),
                });
                continue;
            }
            let mut ignored = Vec::new();
            let mut counted = None;
            for s in all.iter().filter(|s| s.context == *ctx) {
                match self.not_counting(m, check, s.reporter_id).await? {
                    None => {
                        counted = Some(s);
                        break;
                    }
                    Some(why) => ignored.push(format!(
                        "the report by {} doesn't count: {why}",
                        s.reporter.as_deref().unwrap_or("a deleted user")
                    )),
                }
            }
            let by = |s: &Status| {
                s.reporter
                    .clone()
                    .unwrap_or_else(|| "a deleted user".to_owned())
            };
            out.push(match counted {
                Some(s) if s.state == "success" => Requirement {
                    key: "statuses",
                    state: "met",
                    tone: "done",
                    reason: format!("{ctx}: success, reported by {}.", by(s)),
                    fix: None,
                },
                Some(s) if s.state == "pending" => Requirement {
                    key: "statuses",
                    state: "pending",
                    tone: "waiting",
                    reason: format!("{ctx}: pending, reported by {}.", by(s)),
                    fix: Some("Wait for the check".to_owned()),
                },
                Some(s) => Requirement {
                    key: "statuses",
                    state: "unmet",
                    tone: "blocking",
                    reason: format!("{ctx}: {}, reported by {}.", s.state, by(s)),
                    fix: Some("Fix what the check found and push again".to_owned()),
                },
                None if ignored.is_empty() => Requirement {
                    key: "statuses",
                    state: "pending",
                    tone: "waiting",
                    reason: format!("{ctx}: no status yet."),
                    fix: Some("Wait for the check".to_owned()),
                },
                None => Requirement {
                    key: "statuses",
                    state: "unmet",
                    tone: "blocking",
                    reason: format!("{ctx}: no status counts; {}.", ignored.join("; ")),
                    fix: Some("Have an allowed reporter report it".to_owned()),
                },
            });
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls() {
        assert!(check_url("https://ci.example.org/run/1").is_ok());
        assert!(check_url("http://localhost:8080/x").is_ok());
        for bad in [
            "http://ci.example.org",
            "javascript:alert(1)",
            "ftp://x",
            "not a url",
        ] {
            assert!(check_url(bad).is_err(), "{bad}");
        }
    }
}
