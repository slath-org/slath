// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Facts about a diff: what Slath itself sees in it, as opposed to what
//! anyone claims about it. They're computed from Git alone on every read
//! (nothing is stored), for a merge request revision's whole diff and for
//! each change version's own diff, so Git-made, jj-made, and mixed stacks
//! with the same trees get the same facts. What Git says about a diff
//! depends only on its object IDs, so that part is cached in memory; the
//! repository's sensitive paths, a work order's scope, and the allowlist
//! are applied on every read. Where something can't be computed exactly,
//! `incomplete` says so; a fact is never guessed.

use slath_core::authz::Subject;
use slath_core::facts::{Format, PackageChanges, dependency_file, is_test_path};
use slath_core::ids::RepoId;
use slath_core::secret_scan::glob_match;
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

/// What Git alone says about one file, before the repository's settings
/// (sensitive paths, a scope, the allowlist) are applied.
#[derive(Debug, Clone)]
struct GitFile {
    /// The paths its change touches (a rename: old and new).
    touched: Vec<String>,
    deleted_test: Option<String>,
    added_binary: Option<String>,
    mode_change: Option<ModeChange>,
    dependency: Option<DependencyFact>,
    /// The scanner's findings on its added lines, with fingerprints.
    secrets: Vec<(SecretFact, String)>,
}

/// What Git alone says about a diff. It depends only on the two object
/// IDs (and the instance's limits and rules), so it's cached by them.
#[derive(Debug, Clone, Default)]
pub(crate) struct GitFacts {
    size: Size,
    files: Vec<GitFile>,
    incomplete: Vec<String>,
}

/// A diff: its repository, base (`None`: the empty tree), and head.
type Key = (RepoId, Option<String>, String);

/// The Git part of recent diffs' facts, at most `limits.facts_cache_entries`
/// of them; the oldest goes first. Object IDs never change what they name,
/// so an entry never goes stale.
#[derive(Default)]
pub struct FactsCache {
    entries: std::sync::Mutex<Entries>,
}

/// The cached facts, and their keys oldest first.
type Entries = (
    std::collections::HashMap<Key, std::sync::Arc<GitFacts>>,
    std::collections::VecDeque<Key>,
);

impl FactsCache {
    fn get(&self, k: &Key) -> Option<std::sync::Arc<GitFacts>> {
        self.entries.lock().ok()?.0.get(k).cloned()
    }

    fn put(&self, k: Key, v: std::sync::Arc<GitFacts>, max: usize) {
        let Ok(mut e) = self.entries.lock() else {
            return;
        };
        let (map, order) = &mut *e;
        if max == 0 || map.contains_key(&k) {
            return;
        }
        while map.len() >= max {
            match order.pop_front() {
                Some(old) => {
                    map.remove(&old);
                }
                None => break,
            }
        }
        order.push_back(k.clone());
        map.insert(k, v);
    }
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
        let git = self.git_facts(repo, base, head).await?;
        let sensitive = self.sensitive_paths(repo).await?;
        let allowlist = self.allowlist(repo, default_branch).await;
        let in_scope = |p: &str| {
            scope.is_none_or(|globs| globs.iter().any(|g| glob_match(g.as_bytes(), p.as_bytes())))
        };
        let mut facts = Facts {
            size: git.size.clone(),
            incomplete: git.incomplete.clone(),
            ..Facts::default()
        };
        let mut outside = Vec::new();
        for f in &git.files {
            for p in &f.touched {
                if sensitive.matches(p.as_bytes()) && !facts.sensitive_paths.contains(p) {
                    facts.sensitive_paths.push(p.clone());
                }
                if !in_scope(p) && !outside.contains(p) {
                    outside.push(p.clone());
                }
            }
            facts.deleted_tests.extend(f.deleted_test.clone());
            facts.added_binaries.extend(f.added_binary.clone());
            facts.mode_changes.extend(f.mode_change.clone());
            facts.dependencies.extend(f.dependency.clone());
            facts.secrets.extend(
                f.secrets
                    .iter()
                    .filter(|(x, print)| !allowlist.allows(Some(&x.path), print))
                    .map(|(x, _)| x.clone()),
            );
        }
        facts.outside_scope = scope.map(|_| outside);
        Ok(facts)
    }

    /// The Git part of `base..head`'s facts, from the cache or computed.
    async fn git_facts(
        &self,
        repo: RepoId,
        base: Option<&ObjectId>,
        head: &ObjectId,
    ) -> Result<std::sync::Arc<GitFacts>> {
        let key: Key = (
            repo,
            base.map(|b| b.as_str().to_owned()),
            head.as_str().to_owned(),
        );
        if let Some(hit) = self.facts_cache.get(&key) {
            return Ok(hit);
        }
        let computed = std::sync::Arc::new(self.compute_git_facts(repo, base, head).await?);
        self.facts_cache.put(
            key,
            computed.clone(),
            self.config.limits.facts_cache_entries,
        );
        Ok(computed)
    }

    async fn compute_git_facts(
        &self,
        repo: RepoId,
        base: Option<&ObjectId>,
        head: &ObjectId,
    ) -> Result<GitFacts> {
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
        let mut git = GitFacts::default();
        if cut {
            // Too big to read: only the paths and the size are known.
            let (files, additions, deletions) =
                slath_git::browse::shortstat(&self.runner, &dir, base, head)
                    .await
                    .map_err(ServiceError::internal)?;
            git.size = Size {
                files,
                additions,
                deletions,
            };
            let paths = slath_git::browse::changed_paths(&self.runner, &dir, base, head)
                .await
                .map_err(ServiceError::internal)?;
            git.files = paths
                .iter()
                .map(|p| GitFile {
                    touched: vec![text(p)],
                    deleted_test: None,
                    added_binary: None,
                    mode_change: None,
                    dependency: None,
                    secrets: Vec::new(),
                })
                .collect();
            git.incomplete.push(format!(
                "The diff is larger than {} MiB: only its paths and size were checked.",
                MAX_DIFF_BYTES / 1024 / 1024
            ));
            return Ok(git);
        }
        let diff = slath_git::diff::parse(&raw, usize::MAX, usize::MAX);
        let mut reader = ObjectReader::open(&self.runner, &dir)
            .await
            .map_err(ServiceError::internal)?;
        let mut scanned: u64 = 0;
        let scan_max = self.config.limits.secret_scan_push_max_bytes.get();
        git.size.files = diff.files.len();
        for f in &diff.files {
            git.size.additions += f.additions as usize;
            git.size.deletions += f.deletions as usize;
            let mode_change = match (&f.old_mode, &f.new_mode) {
                (Some(old), Some(new)) if old != new && f.status != Status::Deleted => {
                    Some(ModeChange {
                        path: text(f.path()),
                        old_mode: Some(old.clone()),
                        new_mode: new.clone(),
                    })
                }
                (None, Some(new))
                    if f.status == Status::Added && (new == "100755" || new == "120000") =>
                {
                    Some(ModeChange {
                        path: text(f.path()),
                        old_mode: None,
                        new_mode: new.clone(),
                    })
                }
                _ => None,
            };
            let dependency = self
                .dependency_fact(&mut reader, f, &mut git.incomplete)
                .await?;
            let mut file = GitFile {
                touched: touched(f).into_iter().map(text).collect(),
                deleted_test: (f.status == Status::Deleted && is_test_path(&f.old_path))
                    .then(|| text(&f.old_path)),
                added_binary: (matches!(f.status, Status::Added | Status::Copied) && f.binary)
                    .then(|| text(f.path())),
                mode_change,
                dependency,
                secrets: Vec::new(),
            };
            let (added, numbers) = added_text(f);
            if !f.binary && !added.is_empty() {
                scanned += added.len() as u64;
                if scanned > scan_max {
                    if !git.incomplete.iter().any(|n| n.contains("secret")) {
                        git.incomplete.push(format!(
                            "Only the first {scan_max} bytes of added text were scanned for \
                             secrets (limits.secret_scan_push_max_bytes)."
                        ));
                    }
                } else {
                    file.secrets = secrets_in(self, f, &added, &numbers);
                }
            }
            git.files.push(file);
        }
        Ok(git)
    }

    /// The fact for `f` if it's a dependency manifest or lockfile.
    async fn dependency_fact(
        &self,
        reader: &mut ObjectReader,
        f: &FileDiff,
        incomplete: &mut Vec<String>,
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
                    incomplete.push(format!(
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
                    incomplete.push(format!(
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

/// The scanner's findings on `f`'s added lines, each with its fingerprint
/// for the allowlist.
fn secrets_in(
    s: &Services,
    f: &FileDiff,
    added: &[u8],
    numbers: &[u32],
) -> Vec<(SecretFact, String)> {
    let path = text(f.path());
    s.scan_rules
        .scan(added)
        .into_iter()
        .map(|hit| {
            (
                SecretFact {
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
                },
                hit.fingerprint,
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(n: u8) -> Key {
        (
            RepoId::from_uuid(uuid::Uuid::nil()),
            None,
            format!("{n:040}"),
        )
    }

    #[test]
    fn the_cache_keeps_the_newest_entries() {
        let c = FactsCache::default();
        for n in 0..3 {
            c.put(key(n), std::sync::Arc::default(), 2);
        }
        assert!(c.get(&key(0)).is_none());
        assert!(c.get(&key(1)).is_some() && c.get(&key(2)).is_some());
        // Putting an entry again keeps it where it is.
        c.put(key(1), std::sync::Arc::default(), 2);
        c.put(key(3), std::sync::Arc::default(), 2);
        assert!(c.get(&key(1)).is_none() && c.get(&key(3)).is_some());
        // Without room, nothing is kept.
        let none = FactsCache::default();
        none.put(key(0), std::sync::Arc::default(), 0);
        assert!(none.get(&key(0)).is_none());
    }
}
