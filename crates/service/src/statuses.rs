// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Commit statuses: reports on a commit by an authenticated reporter
//! (evidence class "reported": Slath knows who reported it, not that the
//! check ran), and which reports satisfy a merge request's required checks
//! (spec/07 commit statuses, spec/05 verdicts and requirements).

use slath_core::authz::{Action, Subject};
use slath_core::ids::UserId;
use slath_db::merge_requests::MergeRequest;
use slath_db::statuses::{Annotation, NewStatus, Status};
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
    pub annotations: Vec<AnnotationInput>,
}

/// A line annotation as reported; every field is checked.
#[derive(Debug, Clone, Default)]
pub struct AnnotationInput {
    pub path: String,
    pub line_start: Option<i32>,
    pub line_end: Option<i32>,
    pub severity: String,
    pub category: Option<String>,
    pub message: String,
}

/// The neutral categories of findings.
pub const CATEGORIES: [&str; 8] = [
    "correctness",
    "security",
    "tests",
    "dependencies",
    "scope",
    "maintainability",
    "docs",
    "other",
];

/// The most lines one annotation may span.
const MAX_LINES: i32 = 500;

/// Checks one annotation's fields (not yet whether its lines exist).
fn check_annotation(i: usize, a: &AnnotationInput, max_bytes: usize) -> Result<Annotation> {
    let bad = |field: &str, msg: &str| {
        ServiceError::invalid(&format!("annotations[{i}].{field}"), "invalid", msg)
    };
    if a.path.is_empty()
        || a.path.len() > 4096
        || a.path.starts_with('/')
        || a.path.chars().any(char::is_control)
        || a.path
            .split('/')
            .any(|c| c.is_empty() || c == "." || c == "..")
    {
        return Err(bad(
            "path",
            "Name a file of the commit, relative to its root.",
        ));
    }
    let start = a
        .line_start
        .ok_or_else(|| bad("line_start", "Give the first line (from 1)."))?;
    let end = a.line_end.unwrap_or(start);
    if start < 1 {
        return Err(bad("line_start", "Lines start at 1."));
    }
    if end < start || end - start >= MAX_LINES {
        return Err(bad(
            "line_end",
            &format!("Use a line at or after line_start, at most {MAX_LINES} lines in all."),
        ));
    }
    if !matches!(a.severity.as_str(), "notice" | "warning" | "failure") {
        return Err(bad("severity", "Use notice, warning, or failure."));
    }
    let category = a.category.as_deref().unwrap_or("other");
    if !CATEGORIES.contains(&category) {
        return Err(bad(
            "category",
            &format!("Use one of: {}.", CATEGORIES.join(", ")),
        ));
    }
    let message = a.message.trim();
    if message.is_empty()
        || message.len() > max_bytes
        || message
            .chars()
            .any(|c| c.is_control() && c != '\n' && c != '\t')
    {
        return Err(bad(
            "message",
            &format!(
                "Write up to {max_bytes} bytes of plain text, without control characters other than newlines and tabs."
            ),
        ));
    }
    Ok(Annotation {
        path: a.path.clone(),
        line_start: start,
        line_end: end,
        severity: a.severity.clone(),
        category: category.to_owned(),
        message: message.to_owned(),
    })
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

/// An annotation as the API and webhooks show it.
#[must_use]
pub fn annotation_json(a: &Annotation) -> serde_json::Value {
    serde_json::json!({
        "path": a.path,
        "line_start": a.line_start,
        "line_end": a.line_end,
        "severity": a.severity,
        "category": a.category,
        "message": a.message,
    })
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
        let max = self.config.limits.status_annotations;
        if s.annotations.len() > max {
            return Err(ServiceError::invalid(
                "annotations",
                "too_many",
                format!("Report at most {max} annotations per status (limits.status_annotations)."),
            ));
        }
        let max_bytes = self.config.limits.status_annotation_max_bytes.as_usize();
        let annotations = s
            .annotations
            .iter()
            .enumerate()
            .map(|(i, a)| check_annotation(i, a, max_bytes))
            .collect::<Result<Vec<_>>>()?;
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
        self.check_annotated_lines(a.repo.id, &oid, &annotations)
            .await?;
        if slath_db::statuses::count(&self.pool, a.repo.id, oid.as_str(), context).await?
            >= MAX_PER_CONTEXT
        {
            return Err(ServiceError::invalid(
                "context",
                "too_many",
                format!("This commit already has {MAX_PER_CONTEXT} statuses for this context."),
            ));
        }
        let mut tx = self.pool.begin().await.map_err(ServiceError::internal)?;
        let id = slath_db::statuses::insert(
            &mut tx,
            &NewStatus {
                repo: a.repo.id,
                commit_oid: oid.as_str(),
                context,
                state: &s.state,
                description,
                target_url,
                reporter: *reporter,
                annotations: &annotations,
            },
        )
        .await?;
        tx.commit().await.map_err(ServiceError::internal)?;
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
                "annotations": annotations.iter().map(annotation_json).collect::<Vec<_>>(),
            }}),
        )
        .await;
        slath_db::statuses::for_commit(&self.pool, a.repo.id, oid.as_str())
            .await?
            .into_iter()
            .find(|s| s.id == id)
            .ok_or(ServiceError::NotFound)
    }

    /// Fails unless every annotation's lines exist at the commit.
    async fn check_annotated_lines(
        &self,
        repo: slath_core::ids::RepoId,
        oid: &ObjectId,
        annotations: &[Annotation],
    ) -> Result<()> {
        let dir = self.repo_path(repo);
        let mut lines = std::collections::HashMap::<&str, Option<u32>>::new();
        for (i, a) in annotations.iter().enumerate() {
            let n = match lines.get(a.path.as_str()) {
                Some(n) => *n,
                None => {
                    let n =
                        slath_git::linemap::line_count(&self.runner, &dir, oid, a.path.as_bytes())
                            .await
                            .map_err(ServiceError::internal)?;
                    lines.insert(&a.path, n);
                    n
                }
            };
            let Some(n) = n else {
                return Err(ServiceError::invalid(
                    &format!("annotations[{i}].path"),
                    "not_found",
                    "The commit has no such file.",
                ));
            };
            if u32::try_from(a.line_end).map_or(true, |e| e > n) {
                return Err(ServiceError::invalid(
                    &format!("annotations[{i}].line_end"),
                    "invalid",
                    format!("The file has {n} lines at this commit."),
                ));
            }
        }
        Ok(())
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

    fn note(path: &str, start: i32, end: Option<i32>, message: &str) -> AnnotationInput {
        AnnotationInput {
            path: path.to_owned(),
            line_start: Some(start),
            line_end: end,
            severity: "warning".to_owned(),
            category: None,
            message: message.to_owned(),
        }
    }

    #[test]
    fn annotations() {
        let ok = check_annotation(0, &note("src/a.rs", 3, None, " unused \n"), 100).unwrap();
        assert_eq!((ok.line_start, ok.line_end), (3, 3));
        assert_eq!(ok.category, "other");
        assert_eq!(ok.message, "unused");
        assert!(check_annotation(0, &note("a", 1, Some(500), "x"), 100).is_ok());
        let field = |e: ServiceError| match e {
            ServiceError::Invalid(f) => f[0].field.clone(),
            other => panic!("{other:?}"),
        };
        for (n, field_name) in [
            (note("", 1, None, "x"), "annotations[2].path"),
            (note("/etc/passwd", 1, None, "x"), "annotations[2].path"),
            (note("a/../b", 1, None, "x"), "annotations[2].path"),
            (note("a//b", 1, None, "x"), "annotations[2].path"),
            (note("a", 0, None, "x"), "annotations[2].line_start"),
            (note("a", 5, Some(4), "x"), "annotations[2].line_end"),
            (note("a", 1, Some(501), "x"), "annotations[2].line_end"),
            (note("a", 1, None, "  "), "annotations[2].message"),
            (
                note("a", 1, None, &"x".repeat(101)),
                "annotations[2].message",
            ),
            (note("a", 1, None, "a\u{1b}[31m"), "annotations[2].message"),
        ] {
            assert_eq!(field(check_annotation(2, &n, 100).unwrap_err()), field_name);
        }
        let mut n = note("a", 1, None, "x");
        n.severity = "error".to_owned();
        assert_eq!(
            field(check_annotation(0, &n, 100).unwrap_err()),
            "annotations[0].severity"
        );
        n.severity = "notice".to_owned();
        n.category = Some("style".to_owned());
        assert_eq!(
            field(check_annotation(0, &n, 100).unwrap_err()),
            "annotations[0].category"
        );
        n.category = Some("security".to_owned());
        assert_eq!(check_annotation(0, &n, 100).unwrap().category, "security");
    }

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
