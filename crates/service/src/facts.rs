// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Facts about a diff: what Slath itself sees in it, as opposed to what
//! anyone claims about it. They're computed from Git alone on every read
//! (nothing is stored), for a merge request revision's whole diff and for
//! each change version's own diff, so Git-made, jj-made, and mixed stacks
//! with the same trees get the same facts. Where something can't be
//! computed exactly, `incomplete` says so; a fact is never guessed.

use slath_core::authz::Subject;
use slath_core::facts::{Format, PackageChanges, dependency_file, is_test_path};
use slath_core::ids::RepoId;
use slath_core::secret_scan::{Allowlist, glob_match};
use slath_git::ObjectId;
use slath_git::diff::{FileDiff, LineKind, Status};
use slath_git::object::{Blob, ObjectReader};

use crate::Services;
use crate::error::{Result, ServiceError};

/// The largest raw diff read for facts (as for the diff views).
const MAX_DIFF_BYTES: usize = 64 * 1024 * 1024;

/// How big a diff is.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct Size {
    pub files: usize,
    pub additions: usize,
    pub deletions: usize,
}

/// A dependency manifest or lockfile the diff changes.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct DependencyFact {
    pub path: String,
    pub ecosystem: &'static str,
    /// Whether its packages were compared.
    pub packages: bool,
    #[serde(flatten)]
    pub changes: PackageChanges,
}

/// A file whose mode changed, or an added executable or symbolic link.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ModeChange {
    pub path: String,
    pub old_mode: Option<String>,
    pub new_mode: String,
}

/// A possible secret on an added line; never with its text.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct SecretFact {
    pub path: String,
    pub line: u32,
    pub rule: String,
    /// The rule's description, for people.
    pub description: String,
    /// `warn` or `block`.
    pub tier: &'static str,
}

/// The facts of one diff.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct Facts {
    pub size: Size,
    /// Changed paths no glob of the work order's scope matches; `None`
    /// without a work order or a scope.
    pub outside_scope: Option<Vec<String>>,
    pub sensitive_paths: Vec<String>,
    pub dependencies: Vec<DependencyFact>,
    pub deleted_tests: Vec<String>,
    pub added_binaries: Vec<String>,
    pub mode_changes: Vec<ModeChange>,
    pub secrets: Vec<SecretFact>,
    /// What couldn't be computed exactly, in words.
    pub incomplete: Vec<String>,
}

fn text(p: &[u8]) -> String {
    String::from_utf8_lossy(p).into_owned()
}

/// The paths a file's change touches: a rename or copy both.
fn touched(f: &FileDiff) -> Vec<&[u8]> {
    match f.status {
        Status::Renamed | Status::Copied if f.old_path != f.new_path => {
            vec![&f.old_path, &f.new_path]
        }
        _ => vec![f.path()],
    }
}

/// The file's added lines joined, with each one's line number.
fn added_text(f: &FileDiff) -> (Vec<u8>, Vec<u32>) {
    let mut out = Vec::new();
    let mut numbers = Vec::new();
    for l in f.hunks.iter().flat_map(|h| &h.lines) {
        if l.kind == LineKind::Added {
            out.extend_from_slice(&l.text);
            out.push(b'\n');
            numbers.push(l.new_no.unwrap_or(0));
        }
    }
    (out, numbers)
}

impl Services {
    /// The facts of `base..head` in `repo` (from the empty tree without a
    /// base). `scope` is the work order's path scope, if it has one.
    pub async fn diff_facts(
        &self,
        repo: (RepoId, &str),
        base: Option<&ObjectId>,
        head: &ObjectId,
        scope: Option<&[String]>,
    ) -> Result<Facts> {
        let (repo, default_branch) = repo;
        let dir = self.repo_path(repo);
        let (raw, cut) = slath_git::browse::diff(
            &self.runner,
            &dir,
            base,
            head,
            slath_git::browse::DiffOptions::default(),
            MAX_DIFF_BYTES,
        )
        .await
        .map_err(ServiceError::internal)?;
        let sensitive = self.sensitive_paths(repo).await?;
        let mut facts = Facts::default();
        let in_scope =
            |p: &[u8]| scope.is_none_or(|globs| globs.iter().any(|g| glob_match(g.as_bytes(), p)));
        let mut outside = Vec::new();
        if cut {
            // Too big to read: only the paths and the size are known.
            let (files, additions, deletions) =
                slath_git::browse::shortstat(&self.runner, &dir, base, head)
                    .await
                    .map_err(ServiceError::internal)?;
            facts.size = Size {
                files,
                additions,
                deletions,
            };
            let paths = slath_git::browse::changed_paths(&self.runner, &dir, base, head)
                .await
                .map_err(ServiceError::internal)?;
            for p in &paths {
                if sensitive.matches(p) {
                    facts.sensitive_paths.push(text(p));
                }
                if !in_scope(p) {
                    outside.push(text(p));
                }
            }
            facts.outside_scope = scope.map(|_| outside);
            facts.incomplete.push(format!(
                "The diff is larger than {} MiB: only its paths and size were checked.",
                MAX_DIFF_BYTES / 1024 / 1024
            ));
            return Ok(facts);
        }
        let diff = slath_git::diff::parse(&raw, usize::MAX, usize::MAX);
        let mut reader = ObjectReader::open(&self.runner, &dir)
            .await
            .map_err(ServiceError::internal)?;
        let allowlist = self.allowlist(repo, default_branch).await;
        let mut scanned: u64 = 0;
        let scan_max = self.config.limits.secret_scan_push_max_bytes.get();
        facts.size.files = diff.files.len();
        for f in &diff.files {
            facts.size.additions += f.additions as usize;
            facts.size.deletions += f.deletions as usize;
            for p in touched(f) {
                if sensitive.matches(p) && !facts.sensitive_paths.contains(&text(p)) {
                    facts.sensitive_paths.push(text(p));
                }
                if !in_scope(p) && !outside.contains(&text(p)) {
                    outside.push(text(p));
                }
            }
            if f.status == Status::Deleted && is_test_path(&f.old_path) {
                facts.deleted_tests.push(text(&f.old_path));
            }
            if matches!(f.status, Status::Added | Status::Copied) && f.binary {
                facts.added_binaries.push(text(f.path()));
            }
            match (&f.old_mode, &f.new_mode) {
                (Some(old), Some(new)) if old != new && f.status != Status::Deleted => {
                    facts.mode_changes.push(ModeChange {
                        path: text(f.path()),
                        old_mode: Some(old.clone()),
                        new_mode: new.clone(),
                    });
                }
                (None, Some(new))
                    if f.status == Status::Added && (new == "100755" || new == "120000") =>
                {
                    facts.mode_changes.push(ModeChange {
                        path: text(f.path()),
                        old_mode: None,
                        new_mode: new.clone(),
                    });
                }
                _ => {}
            }
            if let Some(d) = self.dependency_fact(&mut reader, f, &mut facts).await? {
                facts.dependencies.push(d);
            }
            if f.binary {
                continue;
            }
            let (added, numbers) = added_text(f);
            if added.is_empty() {
                continue;
            }
            scanned += added.len() as u64;
            if scanned > scan_max {
                if !facts.incomplete.iter().any(|n| n.contains("secret")) {
                    facts.incomplete.push(format!(
                        "Only the first {scan_max} bytes of added text were scanned for secrets \
                         (limits.secret_scan_push_max_bytes)."
                    ));
                }
                continue;
            }
            push_secrets(&mut facts, self, f, &added, &numbers, &allowlist);
        }
        facts.outside_scope = scope.map(|_| outside);
        Ok(facts)
    }

    /// The fact for `f` if it's a dependency manifest or lockfile.
    async fn dependency_fact(
        &self,
        reader: &mut ObjectReader,
        f: &FileDiff,
        facts: &mut Facts,
    ) -> Result<Option<DependencyFact>> {
        let Some((ecosystem, format)) = dependency_file(f.path()) else {
            return Ok(None);
        };
        let path = text(f.path());
        let mut fact = DependencyFact {
            path: path.clone(),
            ecosystem: ecosystem.as_str(),
            packages: false,
            changes: PackageChanges::default(),
        };
        if format == Format::Unparsed {
            return Ok(Some(fact));
        }
        let max = self.config.limits.facts_file_max_bytes.get();
        let limit = usize::try_from(max).unwrap_or(usize::MAX);
        let mut sides = Vec::new();
        for blob in [&f.old_blob, &f.new_blob] {
            let Some(oid) = blob else {
                sides.push(Some(Default::default()));
                continue;
            };
            let oid = ObjectId::parse(oid).map_err(ServiceError::internal)?;
            let data = match reader
                .read_blob(&oid, limit)
                .await
                .map_err(ServiceError::internal)?
            {
                Blob::Data(d) => d,
                Blob::TooLarge { .. } => {
                    facts.incomplete.push(format!(
                        "{path} is larger than limits.facts_file_max_bytes ({max} bytes): \
                         its packages weren't compared."
                    ));
                    sides.push(None);
                    break;
                }
            };
            match slath_core::facts::packages(format, &data) {
                Ok(p) => sides.push(Some(p)),
                Err(e) => {
                    facts.incomplete.push(format!(
                        "{path} couldn't be read ({e}): its packages weren't compared."
                    ));
                    sides.push(None);
                    break;
                }
            }
        }
        if let [Some(old), Some(new)] = &sides[..] {
            fact.packages = true;
            fact.changes = slath_core::facts::compare(old, new);
        }
        Ok(Some(fact))
    }

    /// The facts of a merge request's revision `revision` (default: the
    /// current one), or of the change version `commit` in it.
    pub async fn mr_facts(
        &self,
        subject: &Subject,
        (ns, repo, number): (&str, &str, i32),
        revision: Option<i32>,
        commit: Option<&str>,
    ) -> Result<Facts> {
        let (a, m) = self.mr(subject, ns, repo, number).await?;
        let (base, head) = self.mr_diff_range(&m, revision, commit).await?;
        self.mr_diff_facts(&a.repo, &m, base.as_ref(), &head).await
    }

    /// The facts of what `m` would land: its current revision's merge base
    /// to `m`'s head (a prefix that lands has a head of its own); `None`
    /// without a revision.
    pub(crate) async fn mr_landing_facts(
        &self,
        repo: &slath_db::repos::Repo,
        m: &slath_db::merge_requests::MergeRequest,
    ) -> Result<Option<Facts>> {
        let Some(rev) = slath_db::merge_requests::revisions(&self.pool, m.id)
            .await?
            .pop()
        else {
            return Ok(None);
        };
        let parse = |s: &str| ObjectId::parse(s).map_err(ServiceError::internal);
        let base = rev.merge_base_oid.as_deref().map(parse).transpose()?;
        let head = parse(m.head_oid.as_deref().unwrap_or(&rev.head_oid))?;
        Ok(Some(
            self.mr_diff_facts(repo, m, base.as_ref(), &head).await?,
        ))
    }

    /// The facts of `base..head` of merge request `m`.
    pub(crate) async fn mr_diff_facts(
        &self,
        repo: &slath_db::repos::Repo,
        m: &slath_db::merge_requests::MergeRequest,
        base: Option<&ObjectId>,
        head: &ObjectId,
    ) -> Result<Facts> {
        let scope = self
            .order_of_mr(m)
            .await?
            .map(|o| o.scope)
            .filter(|s| !s.is_empty());
        self.diff_facts(
            (repo.id, &repo.default_branch),
            base,
            head,
            scope.as_deref(),
        )
        .await
    }
}

/// Adds the scanner's findings on `f`'s added lines, without those the
/// allowlist allows.
fn push_secrets(
    facts: &mut Facts,
    s: &Services,
    f: &FileDiff,
    added: &[u8],
    numbers: &[u32],
    allowlist: &Allowlist,
) {
    let path = text(f.path());
    for hit in s.scan_rules.scan(added) {
        if allowlist.allows(Some(&path), &hit.fingerprint) {
            continue;
        }
        facts.secrets.push(SecretFact {
            path: path.clone(),
            line: hit
                .line
                .checked_sub(1)
                .and_then(|i| numbers.get(i))
                .copied()
                .unwrap_or(0),
            rule: hit.rule,
            description: hit.description,
            tier: hit.tier.as_str(),
        });
    }
}
