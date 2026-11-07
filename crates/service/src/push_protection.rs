// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Push protection: scans what a push adds (blobs, commit and tag
//! messages) for secrets while its objects are in quarantine. Block-rule
//! findings reject the push in `block` mode; the scan is bounded by the
//! `secret_scan_*` limits, and a scan that can't finish rejects the push.

use std::path::Path;
use std::time::Instant;

use slath_core::config::ScanMode;
use slath_core::ids::RepoId;
use slath_core::secret_scan::{Finding, Tier, is_binary};
use slath_git::ObjectId;
use slath_git::object::{Blob, ObjectReader};

use crate::Services;
use crate::error::{Result, ServiceError};

/// Where a finding is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Place {
    File { commit: String, path: String },
    CommitMessage { commit: String },
    TagMessage { tag: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PushFinding {
    pub place: Place,
    pub finding: Finding,
}

/// What scanning a push found.
#[derive(Debug, Clone, Default)]
pub struct ScanReport {
    pub findings: Vec<PushFinding>,
    /// The scan couldn't finish (limit, timeout, error); the push must be
    /// rejected in `block` mode.
    pub incomplete: Option<String>,
}

impl ScanReport {
    pub fn blocking(&self) -> impl Iterator<Item = &PushFinding> {
        self.findings
            .iter()
            .filter(|f| f.finding.tier == Tier::Block)
    }
}

fn short(oid: &str) -> &str {
    &oid[..oid.len().min(12)]
}

/// The message part of a raw commit or tag (after the header block).
fn message(raw: &[u8]) -> &[u8] {
    raw.windows(2)
        .position(|w| w == b"\n\n")
        .map_or(&[][..], |p| &raw[p + 2..])
}

impl Services {
    /// The mode that applies to a repository: its own, or the site's
    /// minimum if that is stricter.
    pub async fn push_protection_mode(&self, repo: RepoId) -> Result<ScanMode> {
        let own = slath_db::repos::push_protection(&self.pool, repo).await?;
        let own = ScanMode::parse(&own).unwrap_or(ScanMode::Block);
        Ok(own.max(self.config.push_protection.minimum))
    }

    /// Sets a repository's own mode (repository settings); audited. The
    /// site's minimum still applies on top of it.
    pub async fn set_push_protection(
        &self,
        subject: &slath_core::authz::Subject,
        a: &crate::repos::RepoAccess,
        mode: ScanMode,
    ) -> Result<()> {
        self.authorize(subject, a, slath_core::authz::Action::RepoSettings)?;
        slath_db::repos::set_push_protection(&self.pool, a.repo.id, mode.as_str()).await?;
        self.audit(
            subject,
            "repo.push_protection",
            crate::audit::Target::repo(*a.repo.id.as_uuid()),
            serde_json::json!({"mode": mode.as_str()}),
        )
        .await;
        Ok(())
    }

    /// Scans what the push with new values `tips` adds to `repo`.
    pub async fn scan_push(
        &self,
        repo: RepoId,
        tips: &[ObjectId],
        quarantine: Option<&Path>,
    ) -> ScanReport {
        let timeout = self.config.limits.secret_scan_timeout.get();
        match tokio::time::timeout(timeout, self.scan_inner(repo, tips, quarantine, false)).await {
            Ok(Ok(r)) => r,
            Ok(Err(e)) => {
                tracing::warn!(error = %e, repo = %repo, "secret scan failed");
                ScanReport {
                    findings: Vec::new(),
                    incomplete: Some("the secret scan failed; try again".into()),
                }
            }
            Err(_) => ScanReport {
                findings: Vec::new(),
                incomplete: Some(format!(
                    "the secret scan took longer than limits.secret_scan_timeout ({} s)",
                    timeout.as_secs()
                )),
            },
        }
    }

    /// After an import: scans the whole imported history and records how
    /// many findings it has. It only reports; the history exists elsewhere.
    pub async fn scan_import(&self, repo: RepoId) -> Result<usize> {
        let out = self
            .runner
            .repo(&self.repo_path(repo))
            .args([
                "for-each-ref",
                "--format=%(objectname)",
                "refs/heads/",
                "refs/tags/",
            ])
            .run()
            .await
            .map_err(ServiceError::internal)?;
        let mut tips: Vec<ObjectId> = out
            .stdout
            .split(|&b| b == b'\n')
            .filter_map(|l| ObjectId::parse(l).ok())
            .collect();
        tips.sort();
        tips.dedup();
        let timeout = self.config.limits.import_timeout.get();
        let report = tokio::time::timeout(timeout, self.scan_inner(repo, &tips, None, true))
            .await
            .map_err(|_| ServiceError::internal("the import's secret scan timed out"))??;
        let n = report.findings.len();
        slath_db::imports::set_secret_findings(
            &self.pool,
            repo,
            i32::try_from(n).unwrap_or(i32::MAX),
        )
        .await?;
        if n > 0 {
            tracing::warn!(repo = %repo, findings = n, "imported history contains possible secrets");
        }
        Ok(n)
    }

    async fn scan_inner(
        &self,
        repo: RepoId,
        tips: &[ObjectId],
        quarantine: Option<&Path>,
        everything: bool,
    ) -> Result<ScanReport> {
        let started = Instant::now();
        let dir = self.repo_path(repo);
        let content =
            slath_git::scan::new_content(&self.runner, &dir, quarantine, tips, everything)
                .await
                .map_err(ServiceError::internal)?;
        let mut reader = ObjectReader::open_in(&self.runner, &dir, quarantine)
            .await
            .map_err(ServiceError::internal)?;
        let file_max = self.config.limits.secret_scan_file_max_bytes.get();
        let push_max = self.config.limits.secret_scan_push_max_bytes.get();
        let mut report = ScanReport::default();
        let mut total: u64 = 0;
        for c in &content.commits {
            let commit = reader
                .read_raw_commit(c)
                .await
                .map_err(ServiceError::internal)?;
            for f in self.scan_rules.scan(&commit.message) {
                report.findings.push(PushFinding {
                    place: Place::CommitMessage {
                        commit: c.to_string(),
                    },
                    finding: f,
                });
            }
        }
        for t in &content.tags {
            let (_, raw) = reader
                .read(t, 1024 * 1024)
                .await
                .map_err(ServiceError::internal)?;
            for f in self.scan_rules.scan(message(&raw)) {
                report.findings.push(PushFinding {
                    place: Place::TagMessage { tag: t.to_string() },
                    finding: f,
                });
            }
        }
        let limit = usize::try_from(file_max).unwrap_or(usize::MAX);
        for b in &content.blobs {
            let path = String::from_utf8_lossy(&b.path).into_owned();
            let data = match reader
                .read_blob(&b.oid, limit)
                .await
                .map_err(ServiceError::internal)?
            {
                Blob::Data(d) => d,
                Blob::TooLarge { size } => {
                    // A large binary file is fine; a large text file can't be checked.
                    let (_, head) = reader
                        .read(&b.oid, usize::MAX)
                        .await
                        .map_err(ServiceError::internal)?;
                    if is_binary(&head) {
                        continue;
                    }
                    report.incomplete = Some(format!(
                        "{path} in commit {} is a {size}-byte text file, more than limits.secret_scan_file_max_bytes ({file_max}) allows to scan",
                        short(b.commit.as_str())
                    ));
                    return Ok(report);
                }
            };
            if is_binary(&data) {
                continue;
            }
            total += data.len() as u64;
            if total > push_max {
                report.incomplete = Some(format!(
                    "the push adds more text than limits.secret_scan_push_max_bytes ({push_max}) allows to scan"
                ));
                return Ok(report);
            }
            for f in self.scan_rules.scan(&data) {
                report.findings.push(PushFinding {
                    place: Place::File {
                        commit: b.commit.to_string(),
                        path: path.clone(),
                    },
                    finding: f,
                });
            }
        }
        tracing::debug!(
            repo = %repo,
            commits = content.commits.len(),
            files = content.blobs.len(),
            bytes = total,
            ms = started.elapsed().as_millis() as u64,
            "secret scan"
        );
        Ok(report)
    }
}

/// The `remote:` lines for a scan: rejections with fix hints for Git and
/// jj (in `block` mode), warnings otherwise.
#[must_use]
pub fn messages(report: &ScanReport, mode: ScanMode) -> Vec<String> {
    let mut out = Vec::new();
    let block = mode == ScanMode::Block;
    let blocking: Vec<&PushFinding> = report.blocking().collect();
    if block && let Some(why) = &report.incomplete {
        out.push(format!("slath: push rejected by push protection: {why}"));
        out.push("slath:   An administrator can raise the limit in [limits].".into());
        return out;
    }
    if block && !blocking.is_empty() {
        out.push(format!(
            "slath: push rejected: {} secret(s) found (push protection)",
            blocking.len()
        ));
        for f in &blocking {
            out.extend(describe(f, "  "));
        }
        let first = blocking.iter().find_map(|f| match &f.place {
            Place::File { commit, .. } | Place::CommitMessage { commit } => {
                Some(short(commit).to_owned())
            }
            Place::TagMessage { .. } => None,
        });
        out.push(
            "slath:   Remove the secret from every commit that has it, revoke it, then push again:"
                .into(),
        );
        if let Some(c) = first {
            out.push(format!(
                "slath:     Git: git rebase -i {c}^ (mark the commit \"edit\", fix it, git commit --amend, git rebase --continue)"
            ));
            out.push(format!(
                "slath:     jj:  jj edit {c} (fix the file; descendants are rebased), then jj git push"
            ));
        }
    }
    let warn: Vec<&PushFinding> = report
        .findings
        .iter()
        .filter(|f| !block || f.finding.tier == Tier::Warn)
        .collect();
    if !warn.is_empty() {
        out.push(format!(
            "slath: warning: {} possible secret(s) (push protection, not blocking):",
            warn.len()
        ));
        for f in warn {
            out.extend(describe(f, "  "));
        }
    }
    if !block && let Some(why) = &report.incomplete {
        out.push(format!(
            "slath: warning: push protection couldn't scan everything: {why}"
        ));
    }
    out
}

fn describe(f: &PushFinding, indent: &str) -> Vec<String> {
    let x = &f.finding;
    let place = match &f.place {
        Place::File { commit, path } => format!("commit {} {path} line {}", short(commit), x.line),
        Place::CommitMessage { commit } => {
            format!("commit {} message line {}", short(commit), x.line)
        }
        Place::TagMessage { tag } => format!("tag {} message line {}", short(tag), x.line),
    };
    vec![
        format!("slath:{indent}{place}: {} ({})", x.description, x.rule),
        format!("slath:{indent}  {}", x.excerpt),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn finding(tier: Tier) -> PushFinding {
        PushFinding {
            place: Place::File {
                commit: "0123456789abcdef0123456789abcdef01234567".into(),
                path: "config/app.env".into(),
            },
            finding: Finding {
                rule: "github-token".into(),
                description: "GitHub token".into(),
                tier,
                line: 3,
                column: 12,
                excerpt: "TOKEN=ghp_…".into(),
                fingerprint: "00".repeat(16),
            },
        }
    }

    #[test]
    fn block_mode_rejects_with_place_rule_excerpt_and_hints() {
        let report = ScanReport {
            findings: vec![finding(Tier::Block)],
            incomplete: None,
        };
        let text = messages(&report, ScanMode::Block).join("\n");
        assert!(text.contains("push rejected: 1 secret(s) found"), "{text}");
        assert!(
            text.contains("commit 0123456789ab config/app.env line 3: GitHub token (github-token)"),
            "{text}"
        );
        assert!(text.contains("TOKEN=ghp_…"));
        assert!(text.contains("git rebase -i 0123456789ab^"));
        assert!(text.contains("jj edit 0123456789ab"));
        // Warn mode only warns.
        let text = messages(&report, ScanMode::Warn).join("\n");
        assert!(
            !text.contains("rejected") && text.contains("warning"),
            "{text}"
        );
    }

    #[test]
    fn incomplete_scans_reject_in_block_mode() {
        let report = ScanReport {
            findings: vec![],
            incomplete: Some("too big".into()),
        };
        assert!(
            messages(&report, ScanMode::Block)[0]
                .contains("push rejected by push protection: too big")
        );
        assert!(messages(&report, ScanMode::Warn)[0].contains("warning"));
    }

    #[test]
    fn messages_come_after_the_headers() {
        assert_eq!(
            message(b"tree x\nauthor y\n\nFix it\n\nbody"),
            b"Fix it\n\nbody"
        );
        assert_eq!(message(b"tree x"), b"");
    }
}
