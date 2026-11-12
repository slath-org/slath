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

impl Place {
    /// The file's path, for a finding in a file.
    #[must_use]
    pub fn path(&self) -> Option<&str> {
        match self {
            Self::File { path, .. } => Some(path),
            _ => None,
        }
    }
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
    /// The page where all findings of this push are allowed at once: for
    /// a first push into an empty repository (administrators), or for a
    /// push with several findings (whoever may allow findings).
    pub accept: Option<String>,
    /// The remembered push, when it has several findings: the allow links
    /// carry it, so the allow page offers allowing all of them.
    pub push: Option<uuid::Uuid>,
}

/// A blocking finding in a fork MR's source head, as the merge request
/// shows it: redacted like everywhere else (rule, place, the excerpt cut to
/// four characters, the fingerprint).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct HeldFinding {
    pub rule: String,
    pub description: String,
    /// `file`, `commit_message`, or `tag_message`.
    pub place: String,
    /// The commit (or tag object) it's in.
    pub object: String,
    pub path: Option<String>,
    pub line: usize,
    pub excerpt: String,
    pub fingerprint: String,
}

/// A fork MR's source head that the target's push protection held back:
/// it wasn't fetched, and the merge request keeps its previous revision.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct HeldSource {
    pub head: String,
    /// The scan couldn't finish (a limit, a timeout, an error).
    pub incomplete: Option<String>,
    pub findings: Vec<HeldFinding>,
}

impl HeldSource {
    fn new(head: &ObjectId, report: &ScanReport) -> Self {
        Self {
            head: head.to_string(),
            incomplete: report.incomplete.clone(),
            findings: report
                .blocking()
                .map(|f| {
                    let (place, object, path) = match &f.place {
                        Place::File { commit, path } => ("file", commit, Some(path.clone())),
                        Place::CommitMessage { commit } => ("commit_message", commit, None),
                        Place::TagMessage { tag } => ("tag_message", tag, None),
                    };
                    HeldFinding {
                        rule: f.finding.rule.clone(),
                        description: f.finding.description.clone(),
                        place: place.to_owned(),
                        object: object.clone(),
                        path,
                        line: f.finding.line,
                        excerpt: f.finding.excerpt.clone(),
                        fingerprint: f.finding.fingerprint.clone(),
                    }
                })
                .collect(),
        }
    }

    /// The record stored with a merge request, if it can be read.
    #[must_use]
    pub fn of(m: &slath_db::merge_requests::MergeRequest) -> Option<Self> {
        m.source_findings
            .as_ref()
            .and_then(|v| serde_json::from_value(v.clone()).ok())
    }
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
        admins_allow: bool,
    ) -> Result<()> {
        self.authorize(subject, a, slath_core::authz::Action::RepoSettings)?;
        let role = if admins_allow { "admin" } else { "write" };
        slath_db::repos::set_push_protection(&self.pool, a.repo.id, mode.as_str()).await?;
        slath_db::repos::set_secret_allowance_role(&self.pool, a.repo.id, role).await?;
        self.audit(
            subject,
            "repo.push_protection",
            crate::audit::Target::repo(*a.repo.id.as_uuid()),
            serde_json::json!({"mode": mode.as_str(), "allowance_role": role}),
        )
        .await;
        Ok(())
    }

    /// Push protection for a fork MR's fetch (D-53): scans what fetching
    /// `head` from `source` would add to the target, with the target's
    /// mode, allowlist, and allowances. In `block` mode, blocking findings
    /// or an incomplete scan hold the head back: it's recorded on the merge
    /// request and `false` is returned. A head that passes clears the
    /// record.
    pub(crate) async fn admit_fetch(
        &self,
        m: &slath_db::merge_requests::MergeRequest,
        source: &Path,
        head: &ObjectId,
    ) -> Result<bool> {
        let mode = self.push_protection_mode(m.repo_id).await?;
        let mut held = None;
        if mode != ScanMode::Off {
            let mut report = self
                .scan_fetch(m.repo_id, std::slice::from_ref(head), source)
                .await;
            let target = slath_db::repos::get(&self.pool, m.repo_id)
                .await?
                .ok_or(ServiceError::NotFound)?;
            self.apply_allowances(m.repo_id, &target.default_branch, &mut report)
                .await?;
            if mode == ScanMode::Block
                && (report.incomplete.is_some() || report.blocking().next().is_some())
            {
                held = Some(HeldSource::new(head, &report));
            }
        }
        let value = held
            .as_ref()
            .map(serde_json::to_value)
            .transpose()
            .map_err(ServiceError::internal)?;
        if value != m.source_findings {
            slath_db::merge_requests::set_source_findings(&self.pool, m.id, value.as_ref()).await?;
        }
        if let Some(h) = &held {
            tracing::info!(
                mr = %m.id,
                head = %h.head,
                findings = h.findings.len(),
                incomplete = h.incomplete.is_some(),
                "push protection held back a fork merge request's source"
            );
        }
        Ok(held.is_none())
    }

    /// After an allowance in `repo`: fetches again the sources of its merge
    /// requests that push protection held back.
    async fn retry_held_back(&self, repo: RepoId) {
        let ids = match slath_db::merge_requests::held_back(&self.pool, repo).await {
            Ok(ids) => ids,
            Err(e) => {
                tracing::warn!(error = %e, "listing held-back merge requests failed");
                return;
            }
        };
        for id in ids {
            let job = slath_db::jobs::NewJob::new(
                "default",
                crate::merge_requests::MR_FETCH,
                serde_json::json!({"mr_id": id.to_string(), "pusher": null}),
            );
            if let Err(e) = slath_db::jobs::enqueue(&self.pool, &job).await {
                tracing::warn!(error = %e, mr = %id, "queueing mr.fetch failed");
            }
        }
    }

    /// True if only repository administrators may allow findings.
    pub async fn allowances_need_admin(&self, repo: RepoId) -> Result<bool> {
        Ok(slath_db::repos::secret_allowance_role(&self.pool, repo).await? == "admin")
    }

    /// Scans what the push with new values `tips` adds to `repo`.
    pub async fn scan_push(
        &self,
        repo: RepoId,
        tips: &[ObjectId],
        quarantine: Option<&Path>,
    ) -> ScanReport {
        self.scan_bounded(&self.runner, repo, tips, quarantine)
            .await
    }

    /// Scans what fetching `tips` from the repository at `source` would add
    /// to `repo`, reading `source`'s objects in place (nothing is fetched).
    pub async fn scan_fetch(&self, repo: RepoId, tips: &[ObjectId], source: &Path) -> ScanReport {
        match self.runner.reading_objects_of(source) {
            Ok(r) => self.scan_bounded(&r, repo, tips, None).await,
            Err(e) => {
                tracing::warn!(error = %e, repo = %repo, "secret scan failed");
                ScanReport {
                    incomplete: Some("the secret scan failed; try again".into()),
                    ..ScanReport::default()
                }
            }
        }
    }

    async fn scan_bounded(
        &self,
        runner: &slath_git::Runner,
        repo: RepoId,
        tips: &[ObjectId],
        quarantine: Option<&Path>,
    ) -> ScanReport {
        let timeout = self.config.limits.secret_scan_timeout.get();
        match tokio::time::timeout(
            timeout,
            self.scan_inner(runner, repo, tips, quarantine, false),
        )
        .await
        {
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
        let report = tokio::time::timeout(
            timeout,
            self.scan_inner(&self.runner, repo, &tips, None, true),
        )
        .await
        .map_err(|_| ServiceError::internal("the import's secret scan timed out"))??;
        // Findings the imported default branch's allowlist covers are the
        // repository's choice, not secrets to revoke.
        let default_branch = slath_db::repos::get(&self.pool, repo)
            .await?
            .map(|r| r.default_branch)
            .unwrap_or_default();
        let list = self.allowlist(repo, &default_branch).await;
        let allowed = report
            .findings
            .iter()
            .filter(|f| list.allows(f.place.path(), &f.finding.fingerprint))
            .count();
        let n = report.findings.len() - allowed;
        let count = |n: usize| i32::try_from(n).unwrap_or(i32::MAX);
        slath_db::imports::set_secret_findings(&self.pool, repo, count(n), count(allowed)).await?;
        if n > 0 {
            tracing::warn!(repo = %repo, findings = n, "imported history contains possible secrets");
        }
        Ok(n)
    }

    async fn scan_inner(
        &self,
        runner: &slath_git::Runner,
        repo: RepoId,
        tips: &[ObjectId],
        quarantine: Option<&Path>,
        everything: bool,
    ) -> Result<ScanReport> {
        let started = Instant::now();
        let dir = self.repo_path(repo);
        let content = slath_git::scan::new_content(runner, &dir, quarantine, tips, everything)
            .await
            .map_err(ServiceError::internal)?;
        let mut reader = ObjectReader::open_in(runner, &dir, quarantine)
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
                !(allowed.contains(&f.finding.fingerprint)
                    || list.allows(f.place.path(), &f.finding.fingerprint))
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

    /// What server-side writes (merge and squash messages, applied
    /// suggestions) add that push protection blocks in `repo`: block-rule
    /// findings in `parts` (each with its file's path, `None` for a
    /// message) that no allowance and no allowlist entry lets through.
    /// Empty unless the repository's mode is `block`.
    pub(crate) async fn blocking_in(
        &self,
        repo: &slath_db::repos::Repo,
        parts: &[(Option<&str>, &[u8])],
    ) -> Result<Vec<Finding>> {
        if self.push_protection_mode(repo.id).await? != ScanMode::Block {
            return Ok(Vec::new());
        }
        let mut found: Vec<(Option<&str>, Finding)> = parts
            .iter()
            .flat_map(|(path, text)| {
                self.scan_rules
                    .scan(text)
                    .into_iter()
                    .filter(|f| f.tier == Tier::Block)
                    .map(move |f| (*path, f))
            })
            .collect();
        if found.is_empty() {
            return Ok(Vec::new());
        }
        let allowed = slath_db::secret_allowances::active(&self.pool, repo.id).await?;
        let list = self.allowlist(repo.id, &repo.default_branch).await;
        found.retain(|(path, f)| {
            !(allowed.contains(&f.fingerprint) || list.allows(*path, &f.fingerprint))
        });
        Ok(found.into_iter().map(|(_, f)| f).collect())
    }

    /// The links for a rejection. Every rejected push is remembered for a
    /// day (its findings' rules, names, places, and fingerprints, never the
    /// secrets), so the allow pages can say what and where each finding
    /// is; a first push into an empty repository, or a push with several
    /// findings, can be allowed in one step.
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
            push: None,
        };
        let empty = slath_db::refs::snapshot(&self.pool, repo.id)
            .await?
            .is_empty();
        let blocking: Vec<serde_json::Value> = report
            .blocking()
            .map(|f| {
                let (place, object, path) = match &f.place {
                    Place::File { commit, path } => ("file", commit, Some(path)),
                    Place::CommitMessage { commit } => ("commit_message", commit, None),
                    Place::TagMessage { tag } => ("tag_message", tag, None),
                };
                serde_json::json!({
                    "fingerprint": f.finding.fingerprint,
                    "rule": f.finding.rule,
                    "description": f.finding.description,
                    "place": place,
                    "object": object,
                    "path": path,
                    "line": f.finding.line,
                })
            })
            .collect();
        let distinct: std::collections::HashSet<&str> = report
            .blocking()
            .map(|f| f.finding.fingerprint.as_str())
            .collect();
        if !blocking.is_empty() {
            let kind = if empty { "first_push" } else { "push" };
            let id = slath_db::secret_allowances::insert_rejection(
                &self.pool,
                repo.id,
                kind,
                &serde_json::Value::Array(blocking),
                time::OffsetDateTime::now_utc() + time::Duration::hours(ACCEPT_HOURS),
            )
            .await?;
            if empty || distinct.len() >= 2 {
                links.accept = Some(format!("{base}/accept/{id}"));
            }
            links.push = (!empty).then_some(id);
        }
        Ok(links)
    }

    /// Whether `subject` may allow findings in `a`'s repository: a human
    /// user with push access, an administrator when the repository says
    /// so; never a bot or a token's own principal.
    fn may_allow(
        &self,
        subject: &slath_core::authz::Subject,
        a: &crate::repos::RepoAccess,
        need_admin: bool,
    ) -> Result<slath_core::ids::UserId> {
        use slath_core::authz::{Action, Subject, UserKind};
        self.authorize(subject, a, Action::RepoPush)?;
        if need_admin && !a.can(subject, Action::RepoSettings) {
            return Err(ServiceError::Forbidden);
        }
        match subject {
            Subject::User {
                id,
                kind: UserKind::Human,
                ..
            } => Ok(*id),
            _ => Err(ServiceError::Forbidden),
        }
    }

    /// The findings of a rejected push with several of them, for the page
    /// that allows them all (whoever may allow findings).
    pub async fn push_findings(
        &self,
        subject: &slath_core::authz::Subject,
        a: &crate::repos::RepoAccess,
        id: uuid::Uuid,
    ) -> Result<Vec<RejectedFinding>> {
        let need_admin = self.allowances_need_admin(a.repo.id).await?;
        self.may_allow(subject, a, need_admin)?;
        match slath_db::secret_allowances::rejection(&self.pool, a.repo.id, id).await? {
            Some((kind, v)) if kind == "push" => Ok(self.findings_of(&v)),
            _ => Err(ServiceError::NotFound),
        }
    }

    /// The rejected findings of a remembered push, by meaning: rule names
    /// fall back to the rule set for records without one.
    fn findings_of(&self, v: &serde_json::Value) -> Vec<RejectedFinding> {
        v.as_array()
            .into_iter()
            .flatten()
            .filter_map(|f| {
                let rule = f["rule"].as_str()?.to_owned();
                let description = f["description"]
                    .as_str()
                    .map(str::to_owned)
                    .or_else(|| self.scan_rules.description(&rule).map(str::to_owned))
                    .unwrap_or_else(|| rule.clone());
                let object = f["object"].as_str().map(short).unwrap_or_default();
                let line = f["line"].as_u64().unwrap_or(0);
                let place = match (f["place"].as_str(), f["path"].as_str()) {
                    (Some("file"), Some(path)) => {
                        Some(format!("{path} line {line} in commit {object}"))
                    }
                    (Some("commit_message"), _) => {
                        Some(format!("message of commit {object}, line {line}"))
                    }
                    (Some("tag_message"), _) => {
                        Some(format!("message of tag {object}, line {line}"))
                    }
                    _ => None,
                };
                Some(RejectedFinding {
                    fingerprint: f["fingerprint"].as_str()?.to_owned(),
                    rule,
                    description,
                    place,
                })
            })
            .collect()
    }

    /// What a rule finds, in words.
    #[must_use]
    pub fn rule_description(&self, rule: &str) -> String {
        self.scan_rules
            .description(rule)
            .map_or_else(|| rule.to_owned(), str::to_owned)
    }

    /// Whether a remembered rejection is a first push (`Some(true)`), a
    /// push with several findings (`Some(false)`), or gone.
    pub async fn rejection_is_first_push(
        &self,
        a: &crate::repos::RepoAccess,
        id: uuid::Uuid,
    ) -> Result<Option<bool>> {
        Ok(
            slath_db::secret_allowances::rejection(&self.pool, a.repo.id, id)
                .await?
                .map(|(kind, _)| kind == "first_push"),
        )
    }

    /// Allows every finding of a rejected push in one step, with one
    /// reason and one period (whoever may allow findings, never bots);
    /// one audit event lists the fingerprints.
    pub async fn allow_push(
        &self,
        subject: &slath_core::authz::Subject,
        a: &crate::repos::RepoAccess,
        id: uuid::Uuid,
        days: i64,
        reason: &str,
    ) -> Result<usize> {
        let findings = self.push_findings(subject, a, id).await?;
        let user = self.may_allow(subject, a, self.allowances_need_admin(a.repo.id).await?)?;
        let errors = period_errors(days, reason);
        if !errors.is_empty() {
            return Err(ServiceError::Invalid(errors));
        }
        let reason = reason.trim();
        let expires = time::OffsetDateTime::now_utc() + time::Duration::days(days);
        let findings = distinct(findings);
        let mut tx = self.pool.begin().await.map_err(ServiceError::internal)?;
        for f in &findings {
            slath_db::secret_allowances::insert(
                &mut *tx,
                a.repo.id,
                &f.fingerprint,
                (&f.rule, Some(&f.description)),
                user,
                (!reason.is_empty()).then_some(reason),
                expires,
            )
            .await?;
        }
        tx.commit().await.map_err(ServiceError::internal)?;
        let fingerprints: Vec<&str> = findings.iter().map(|f| f.fingerprint.as_str()).collect();
        let rules: Vec<&str> = findings.iter().map(|f| f.rule.as_str()).collect();
        self.audit(
            subject,
            "secret_scan.allow_push",
            crate::audit::Target::repo(*a.repo.id.as_uuid()),
            serde_json::json!({
                "fingerprints": fingerprints,
                "rules": rules,
                "days": days,
                "reason": reason,
            }),
        )
        .await;
        self.retry_held_back(a.repo.id).await;
        Ok(findings.len())
    }

    /// Allows one finding in a repository for `days` (writers only, never
    /// bots); audited.
    pub async fn allow_secret(
        &self,
        subject: &slath_core::authz::Subject,
        a: &crate::repos::RepoAccess,
        (fingerprint, rule, push): (&str, &str, Option<uuid::Uuid>),
        days: i64,
        reason: &str,
    ) -> Result<()> {
        let id = &self.may_allow(subject, a, self.allowances_need_admin(a.repo.id).await?)?;
        // What it is, as the rejected push's scan named it.
        let description = match push {
            Some(p) => self
                .push_findings(subject, a, p)
                .await
                .ok()
                .and_then(|fs| fs.into_iter().find(|f| f.fingerprint == fingerprint))
                .map(|f| f.description),
            None => None,
        }
        .unwrap_or_else(|| self.rule_description(rule));
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
        errors.extend(period_errors(days, reason));
        let reason = reason.trim();
        if !errors.is_empty() {
            return Err(ServiceError::Invalid(errors));
        }
        let expires = time::OffsetDateTime::now_utc() + time::Duration::days(days);
        let allowance = slath_db::secret_allowances::insert(
            &self.pool,
            a.repo.id,
            fingerprint,
            (rule, Some(&description)),
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
        self.retry_held_back(a.repo.id).await;
        Ok(())
    }

    /// The remembered findings of a rejected first push, for the accept
    /// page (repository administrators).
    pub async fn first_push_findings(
        &self,
        subject: &slath_core::authz::Subject,
        a: &crate::repos::RepoAccess,
        id: uuid::Uuid,
    ) -> Result<Vec<RejectedFinding>> {
        self.authorize(subject, a, slath_core::authz::Action::RepoSettings)?;
        match slath_db::secret_allowances::rejection(&self.pool, a.repo.id, id).await? {
            Some((kind, v)) if kind == "first_push" => Ok(self.findings_of(&v)),
            _ => Err(ServiceError::NotFound),
        }
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
        let findings = distinct(findings);
        for f in &findings {
            slath_db::secret_allowances::insert(
                &self.pool,
                a.repo.id,
                &f.fingerprint,
                (&f.rule, Some(&f.description)),
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

/// A finding of a rejected push, as the allow pages show it: what it is
/// and where, never the secret.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RejectedFinding {
    pub fingerprint: String,
    pub rule: String,
    /// What it is ("OpenSSH private key", "GitHub token").
    pub description: String,
    /// Where it is ("tests/keys/a.pem line 1 in commit 3f2a9c81d0e4");
    /// `None` for records without it.
    pub place: Option<String>,
}

/// One finding per fingerprint (the same secret in several places is
/// one allowance).
fn distinct(findings: Vec<RejectedFinding>) -> Vec<RejectedFinding> {
    let mut seen = std::collections::HashSet::new();
    findings
        .into_iter()
        .filter(|f| seen.insert(f.fingerprint.clone()))
        .collect()
}

/// What's wrong with an allowance's period and reason.
fn period_errors(days: i64, reason: &str) -> Vec<crate::FieldError> {
    let mut errors = Vec::new();
    if !ALLOWANCE_DAYS.contains(&days) {
        errors.push(crate::FieldError::new(
            "days",
            "invalid",
            "Choose one of the offered periods.",
        ));
    }
    if reason.trim().len() > 500 {
        errors.push(crate::FieldError::new(
            "reason",
            "too_long",
            "Keep the reason under 500 characters.",
        ));
    }
    errors
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
            let push = links
                .push
                .map(|id| format!("&push={id}"))
                .unwrap_or_default();
            let mut seen = std::collections::HashSet::new();
            for f in &blocking {
                if seen.insert(&f.finding.fingerprint) {
                    out.push(format!(
                        "slath:     {}?fingerprint={}&rule={}{push}",
                        links.allow, f.finding.fingerprint, f.finding.rule
                    ));
                }
            }
        }
        if let (Some(accept), Some(_)) = (&links.accept, links.push) {
            out.push(format!(
                "slath:   Or allow all {} findings of this push at once, with one reason and one period:",
                seen_count(&blocking)
            ));
            out.push(format!("slath:     {accept}"));
        } else if let Some(accept) = &links.accept {
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

/// How many different secrets (fingerprints) `findings` have.
fn seen_count(findings: &[&PushFinding]) -> usize {
    findings
        .iter()
        .map(|f| &f.finding.fingerprint)
        .collect::<std::collections::HashSet<_>>()
        .len()
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
