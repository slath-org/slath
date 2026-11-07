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
    /// Findings let through by an allowance or the allowlist, with why.
    pub allowed: Vec<(PushFinding, &'static str)>,
}

/// Links printed with a rejection.
#[derive(Debug, Clone, Default)]
pub struct Links {
    /// The allowance page of the repository; a finding's fingerprint and
    /// rule are appended.
    pub allow: String,
    /// For a first push into an empty repository: the page where an
    /// administrator accepts all its findings at once.
    pub accept: Option<String>,
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
                    incomplete: Some("the secret scan failed; try again".into()),
                    ..ScanReport::default()
                }
            }
            Err(_) => ScanReport {
                findings: Vec::new(),
                allowed: Vec::new(),
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

/// How long an allowance may last, in days.
pub const ALLOWANCE_DAYS: [i64; 3] = [1, 7, 30];
/// How long a rejected first push can be accepted.
const ACCEPT_HOURS: i64 = 24;
/// The allowlist file, read from the default branch only.
pub const ALLOWLIST_PATH: &str = ".slath/secret-scan.toml";

#[derive(serde::Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct AllowlistFile {
    #[serde(default)]
    allow: Vec<AllowEntry>,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct AllowEntry {
    path: Option<String>,
    fingerprint: Option<String>,
    #[serde(default)]
    #[allow(dead_code)]
    reason: Option<String>,
}

/// Parses `.slath/secret-scan.toml`; a broken file allows nothing.
#[must_use]
pub fn parse_allowlist(text: &str) -> slath_core::secret_scan::Allowlist {
    let file: AllowlistFile = toml::from_str(text).unwrap_or_default();
    let mut list = slath_core::secret_scan::Allowlist::default();
    for e in file.allow {
        if let Some(p) = e.path {
            list.paths.push(p);
        }
        if let Some(f) = e.fingerprint {
            list.fingerprints.push(f.to_ascii_lowercase());
        }
    }
    list
}

impl Services {
    /// The allowlist as it stands on the default branch (never from the
    /// pushed commits, so a push can't allow itself).
    async fn allowlist(
        &self,
        repo: RepoId,
        default_branch: &str,
    ) -> slath_core::secret_scan::Allowlist {
        let spec = format!("refs/heads/{default_branch}:{ALLOWLIST_PATH}");
        let out = self
            .runner
            .repo(&self.repo_path(repo))
            .args(["cat-file", "blob"])
            .arg(&spec)
            .output()
            .await;
        match out {
            Ok(o) if o.status.success() && o.stdout.len() <= 64 * 1024 => {
                parse_allowlist(&String::from_utf8_lossy(&o.stdout))
            }
            _ => slath_core::secret_scan::Allowlist::default(),
        }
    }

    /// Moves findings that an allowance or the allowlist lets through out
    /// of the report's findings.
    pub(crate) async fn apply_allowances(
        &self,
        repo: RepoId,
        default_branch: &str,
        report: &mut ScanReport,
    ) -> Result<()> {
        if report.findings.is_empty() {
            return Ok(());
        }
        let allowed = slath_db::secret_allowances::active(&self.pool, repo).await?;
        let list = self.allowlist(repo, default_branch).await;
        let (keep, pass): (Vec<_>, Vec<_>) = std::mem::take(&mut report.findings)
            .into_iter()
            .partition(|f| {
                let path = match &f.place {
                    Place::File { path, .. } => Some(path.as_str()),
                    _ => None,
                };
                !(allowed.contains(&f.finding.fingerprint)
                    || list.allows(path, &f.finding.fingerprint))
            });
        report.findings = keep;
        for f in pass {
            let why = if allowed.contains(&f.finding.fingerprint) {
                "allowance"
            } else {
                "allowlist on the default branch"
            };
            report.allowed.push((f, why));
        }
        Ok(())
    }

    /// The links for a rejection; for a first push into an empty
    /// repository, remembers its findings so an administrator can accept
    /// them.
    pub(crate) async fn rejection_links(
        &self,
        repo: &slath_db::repos::Repo,
        report: &ScanReport,
    ) -> Result<Links> {
        let base = self
            .config
            .server
            .public_url
            .join(&format!("/{}/{}/-/secret-scan", repo.namespace, repo.name));
        let mut links = Links {
            allow: format!("{base}/allow"),
            accept: None,
        };
        let empty = slath_db::refs::snapshot(&self.pool, repo.id)
            .await?
            .is_empty();
        let blocking: Vec<serde_json::Value> = report
            .blocking()
            .map(|f| serde_json::json!({"fingerprint": f.finding.fingerprint, "rule": f.finding.rule}))
            .collect();
        if empty && !blocking.is_empty() {
            let id = slath_db::secret_allowances::insert_rejection(
                &self.pool,
                repo.id,
                &serde_json::Value::Array(blocking),
                time::OffsetDateTime::now_utc() + time::Duration::hours(ACCEPT_HOURS),
            )
            .await?;
            links.accept = Some(format!("{base}/accept/{id}"));
        }
        Ok(links)
    }

    /// Allows one finding in a repository for `days` (writers only, never
    /// bots); audited.
    pub async fn allow_secret(
        &self,
        subject: &slath_core::authz::Subject,
        a: &crate::repos::RepoAccess,
        fingerprint: &str,
        rule: &str,
        days: i64,
        reason: &str,
    ) -> Result<()> {
        use slath_core::authz::{Subject, UserKind};
        self.authorize(subject, a, slath_core::authz::Action::RepoPush)?;
        let Subject::User {
            id,
            kind: UserKind::Human,
            ..
        } = subject
        else {
            return Err(ServiceError::Forbidden);
        };
        let mut errors = Vec::new();
        if fingerprint.len() != 32
            || !fingerprint
                .bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
        {
            errors.push(crate::FieldError::new(
                "fingerprint",
                "invalid",
                "This link is incomplete; copy it from the push output again.",
            ));
        }
        if rule.is_empty()
            || rule.len() > 100
            || !rule
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        {
            errors.push(crate::FieldError::new(
                "rule",
                "invalid",
                "This link is incomplete; copy it from the push output again.",
            ));
        }
        if !ALLOWANCE_DAYS.contains(&days) {
            errors.push(crate::FieldError::new(
                "days",
                "invalid",
                "Choose one of the offered periods.",
            ));
        }
        let reason = reason.trim();
        if reason.len() > 500 {
            errors.push(crate::FieldError::new(
                "reason",
                "too_long",
                "Keep the reason under 500 characters.",
            ));
        }
        if !errors.is_empty() {
            return Err(ServiceError::Invalid(errors));
        }
        let expires = time::OffsetDateTime::now_utc() + time::Duration::days(days);
        let allowance = slath_db::secret_allowances::insert(
            &self.pool,
            a.repo.id,
            fingerprint,
            rule,
            *id,
            (!reason.is_empty()).then_some(reason),
            expires,
        )
        .await?;
        self.audit(
            subject,
            "secret_scan.allow",
            crate::audit::Target::new("secret_allowance", Some(allowance)).in_repo(*a.repo.id.as_uuid()),
            serde_json::json!({"fingerprint": fingerprint, "rule": rule, "days": days, "reason": reason}),
        )
        .await;
        Ok(())
    }

    /// The remembered findings of a rejected first push, for the accept
    /// page (repository administrators).
    pub async fn first_push_findings(
        &self,
        subject: &slath_core::authz::Subject,
        a: &crate::repos::RepoAccess,
        id: uuid::Uuid,
    ) -> Result<Vec<(String, String)>> {
        self.authorize(subject, a, slath_core::authz::Action::RepoSettings)?;
        let Some(v) = slath_db::secret_allowances::rejection(&self.pool, a.repo.id, id).await?
        else {
            return Err(ServiceError::NotFound);
        };
        Ok(v.as_array()
            .into_iter()
            .flatten()
            .filter_map(|f| {
                Some((
                    f["fingerprint"].as_str()?.to_owned(),
                    f["rule"].as_str()?.to_owned(),
                ))
            })
            .collect())
    }

    /// Accepts every finding of a rejected first push in one step (the
    /// history already exists elsewhere): an allowance per finding for a
    /// day. Repository administrators only, never bots; audited.
    pub async fn accept_first_push(
        &self,
        subject: &slath_core::authz::Subject,
        a: &crate::repos::RepoAccess,
        id: uuid::Uuid,
    ) -> Result<usize> {
        use slath_core::authz::{Subject, UserKind};
        let findings = self.first_push_findings(subject, a, id).await?;
        let Subject::User {
            id: user,
            kind: UserKind::Human,
            ..
        } = subject
        else {
            return Err(ServiceError::Forbidden);
        };
        let expires = time::OffsetDateTime::now_utc() + time::Duration::days(1);
        for (fp, rule) in &findings {
            slath_db::secret_allowances::insert(
                &self.pool,
                a.repo.id,
                fp,
                rule,
                *user,
                Some("accepted with the repository's first push"),
                expires,
            )
            .await?;
        }
        self.audit(
            subject,
            "secret_scan.accept_first_push",
            crate::audit::Target::repo(*a.repo.id.as_uuid()),
            serde_json::json!({"findings": findings.len()}),
        )
        .await;
        Ok(findings.len())
    }

    /// Deletes expired first-push rejections, and allowances expired for
    /// more than 30 days. Run periodically.
    pub async fn purge_secret_scan(&self) -> Result<u64> {
        Ok(slath_db::secret_allowances::purge(&self.pool).await?)
    }

    /// Allowances in force (repository writers).
    pub async fn secret_allowances(
        &self,
        subject: &slath_core::authz::Subject,
        a: &crate::repos::RepoAccess,
    ) -> Result<Vec<slath_db::secret_allowances::Allowance>> {
        self.authorize(subject, a, slath_core::authz::Action::RepoPush)?;
        Ok(slath_db::secret_allowances::list(&self.pool, a.repo.id).await?)
    }
}

/// The `remote:` lines for a scan: rejections with fix hints for Git and
/// jj and the allowance links (in `block` mode), warnings otherwise.
#[must_use]
pub fn messages(report: &ScanReport, mode: ScanMode, links: &Links) -> Vec<String> {
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
        if !links.allow.is_empty() {
            out.push("slath:   Not a secret? A repository writer can allow it, then push again unchanged:".into());
            let mut seen = std::collections::HashSet::new();
            for f in &blocking {
                if seen.insert(&f.finding.fingerprint) {
                    out.push(format!(
                        "slath:     {}?fingerprint={}&rule={}",
                        links.allow, f.finding.fingerprint, f.finding.rule
                    ));
                }
            }
        }
        if let Some(accept) = &links.accept {
            out.push("slath:   This is the repository's first push. If its history already exists elsewhere, an administrator can accept all its findings at once:".into());
            out.push(format!("slath:     {accept}"));
        }
    }
    for (f, why) in &report.allowed {
        out.push(format!("slath: allowed ({why}):"));
        out.extend(describe(f, "  "));
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
            ..ScanReport::default()
        };
        let text = messages(&report, ScanMode::Block, &Links::default()).join("\n");
        assert!(text.contains("push rejected: 1 secret(s) found"), "{text}");
        assert!(
            text.contains("commit 0123456789ab config/app.env line 3: GitHub token (github-token)"),
            "{text}"
        );
        assert!(text.contains("TOKEN=ghp_…"));
        assert!(text.contains("git rebase -i 0123456789ab^"));
        assert!(text.contains("jj edit 0123456789ab"));
        // Warn mode only warns.
        let text = messages(&report, ScanMode::Warn, &Links::default()).join("\n");
        assert!(
            !text.contains("rejected") && text.contains("warning"),
            "{text}"
        );
    }

    #[test]
    fn incomplete_scans_reject_in_block_mode() {
        let report = ScanReport {
            incomplete: Some("too big".into()),
            ..ScanReport::default()
        };
        assert!(
            messages(&report, ScanMode::Block, &Links::default())[0]
                .contains("push rejected by push protection: too big")
        );
        assert!(messages(&report, ScanMode::Warn, &Links::default())[0].contains("warning"));
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
