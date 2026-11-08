// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Changes of merge requests: binding each commit of a new revision to a
//! change (spec: change identity assignment), the stack as the UI and API
//! show it, and rebinding a commit by hand (resolve, split, join).

use std::collections::{HashMap, HashSet};
use std::path::Path;

use slath_core::authz::{Action, Subject};
use slath_core::change_identity::{self, Alias, AliasKind, Binding, Commit, Previous};
use slath_core::ids::{MergeRequestId, UserId};
use slath_db::merge_requests::MergeRequest;
use slath_db::mr_changes::{self as db, NewVersion};
use slath_git::ObjectId;
use slath_git::changes::StackCommit;

use crate::Services;
use crate::error::{Result, ServiceError};
use crate::repos::RepoAccess;

/// At most this much patch text is read per revision for matching.
const MAX_PATCH_BYTES: usize = 32 * 1024 * 1024;

/// The bindings of a new revision, computed before its transaction.
pub(crate) struct Plan {
    commits: Vec<(StackCommit, Vec<Alias>, Binding<uuid::Uuid>)>,
    /// Changes with a version in the previous per-change revision.
    previous: Vec<uuid::Uuid>,
    /// Display IDs already used in the merge request.
    taken: HashSet<String>,
}

/// One row of the stack: a version of the current revision.
#[derive(Debug, Clone)]
pub struct StackEntry {
    pub position: i32,
    pub commit_oid: String,
    /// The change's display ID; `None` while unresolved.
    pub display_id: Option<String>,
    /// `new`, `confirmed`, `inferred`, or `unresolved`.
    pub binding: String,
    pub reason: Option<String>,
    /// Display IDs of the candidates, best first.
    pub candidates: Vec<String>,
    pub conflicted: bool,
    /// Earlier versions of its change (0 for a new change).
    pub earlier_versions: usize,
    /// Other open merge requests that bound one of its aliases.
    pub also_in: Vec<i32>,
}

/// The current revision's stack and the removed changes.
#[derive(Debug, Clone, Default)]
pub struct Stack {
    /// `None` without revisions; whole-only revisions have no entries.
    pub revision: Option<i32>,
    pub entries: Vec<StackEntry>,
    /// Display IDs of changes without a version in the current revision.
    pub removed: Vec<String>,
}

impl Stack {
    /// Merge is blocked while a commit is unresolved or conflicted.
    #[must_use]
    pub fn blocks_merge(&self) -> bool {
        self.entries
            .iter()
            .any(|e| e.binding == "unresolved" || e.conflicted)
    }
}

/// What a revision changed compared with the previous one, for the
/// timeline ("rebased onto main, 2 changes modified, 1 added").
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RevisionSummary {
    /// The merge base moved.
    pub rebased: bool,
    pub unchanged: usize,
    pub modified: usize,
    pub added: usize,
    pub removed: usize,
    pub unresolved: usize,
}

/// What a commit of the current revision is bound to by hand.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Rebind {
    /// Continue this change (display ID): resolve or join.
    Change(String),
    /// A new change: resolve or split.
    New,
}

fn binding_row(b: &Binding<uuid::Uuid>) -> (&'static str, Option<&'static str>, Vec<uuid::Uuid>) {
    match b {
        Binding::Alias(_) => ("confirmed", Some("alias"), Vec::new()),
        Binding::Patch(_) => ("confirmed", Some("patch"), Vec::new()),
        Binding::Inferred(_) => ("inferred", Some("content"), Vec::new()),
        Binding::Unresolved { reason, candidates } => {
            ("unresolved", Some(reason.as_str()), candidates.clone())
        }
        Binding::New { candidates } => ("new", None, candidates.clone()),
    }
}

fn alias_of(kind: &str, value: &str) -> Option<Alias> {
    Some(Alias {
        kind: AliasKind::parse(kind)?,
        value: value.to_owned(),
    })
}

fn aliases_of(c: &StackCommit) -> Vec<Alias> {
    change_identity::aliases(c.change_id.as_deref(), c.trailer.as_deref(), c.oid.as_str())
}

impl Services {
    /// Matches the commits of a new revision (bottom first) to the merge
    /// request's changes.
    pub(crate) async fn plan_changes(
        &self,
        m: &MergeRequest,
        dir: &Path,
        commits: &[ObjectId],
    ) -> Result<Plan> {
        let stack = slath_git::changes::stack(&self.runner, dir, commits, MAX_PATCH_BYTES)
            .await
            .map_err(ServiceError::internal)?;
        let bound: HashMap<Alias, uuid::Uuid> = db::aliases(&self.pool, m.id)
            .await?
            .into_iter()
            .filter_map(|a| Some((alias_of(&a.kind, &a.value)?, a.change_id)))
            .collect();
        let versions = db::versions(&self.pool, m.id).await?;
        let last = versions.iter().map(|v| v.revision_seq).max();
        let prev: Vec<&db::Version> = versions
            .iter()
            .filter(|v| Some(v.revision_seq) == last && v.change_id.is_some())
            .collect();
        let prev_oids: Vec<ObjectId> = prev
            .iter()
            .filter_map(|v| ObjectId::parse(&v.commit_oid).ok())
            .collect();
        let prev_patches: HashMap<String, Vec<u8>> =
            slath_git::changes::stack(&self.runner, dir, &prev_oids, MAX_PATCH_BYTES)
                .await
                .map_err(ServiceError::internal)?
                .into_iter()
                .map(|c| (c.oid.as_str().to_owned(), c.patch))
                .collect();
        let previous: Vec<Previous<uuid::Uuid>> = prev
            .iter()
            .filter_map(|v| {
                Some(Previous {
                    change: v.change_id?,
                    patch_id: v.patch_id.clone(),
                    patch: change_identity::patch_lines(
                        prev_patches
                            .get(&v.commit_oid)
                            .map_or(&[][..], Vec::as_slice),
                    ),
                })
            })
            .collect();
        let input: Vec<Commit> = stack
            .iter()
            .map(|c| Commit {
                aliases: aliases_of(c),
                trailer: c
                    .trailer
                    .as_deref()
                    .and_then(change_identity::trailer_alias),
                patch_id: c.patch_id.clone(),
                patch: change_identity::patch_lines(&c.patch),
            })
            .collect();
        let bindings = change_identity::assign(&input, &bound, &previous);
        let taken = db::changes(&self.pool, m.id)
            .await?
            .into_iter()
            .map(|c| c.display_id)
            .collect();
        Ok(Plan {
            commits: stack
                .into_iter()
                .zip(input)
                .zip(bindings)
                .map(|((c, i), b)| (c, i.aliases, b))
                .collect(),
            previous: previous.iter().map(|p| p.change).collect(),
            taken,
        })
    }

    /// Records the planned versions of revision `seq` (inside its
    /// transaction): new changes, versions, alias bindings, and which
    /// changes are removed.
    pub(crate) async fn apply_changes(
        &self,
        tx: &mut sqlx::PgConnection,
        mr: MergeRequestId,
        (revision, seq): (uuid::Uuid, i32),
        pusher: Option<UserId>,
        mut plan: Plan,
    ) -> Result<()> {
        let mut matched: HashSet<uuid::Uuid> = HashSet::new();
        for (pos, (c, aliases, binding)) in plan.commits.iter().enumerate() {
            let (state, reason, candidates) = binding_row(binding);
            let change = match binding {
                Binding::Unresolved { candidates, .. } => {
                    matched.extend(candidates.iter().copied());
                    None
                }
                Binding::New { .. } => {
                    let wanted = change_identity::display_id(aliases, c.oid.as_str())
                        .ok_or_else(|| ServiceError::internal("no display ID"))?;
                    let display = if plan.taken.contains(&wanted) {
                        change_identity::predict(c.oid.as_str())
                            .ok_or_else(|| ServiceError::internal("no display ID"))?
                    } else {
                        wanted
                    };
                    plan.taken.insert(display.clone());
                    Some(db::insert_change(&mut *tx, mr, &display).await?)
                }
                b => b.change().copied(),
            };
            if let Some(k) = change {
                matched.insert(k);
                db::set_change_state(&mut *tx, k, "active").await?;
                for a in aliases {
                    db::bind_alias(&mut *tx, mr, (a.kind.as_str(), &a.value), k, seq, pusher)
                        .await?;
                }
            }
            db::insert_version(
                &mut *tx,
                &NewVersion {
                    revision,
                    change,
                    position: i32::try_from(pos).map_err(ServiceError::internal)?,
                    commit_oid: c.oid.as_str(),
                    parent_oid: c.parent.as_ref().map(ObjectId::as_str),
                    patch_id: c.patch_id.as_deref(),
                    state,
                    reason,
                    candidates: &candidates,
                    conflicted: c.conflicted,
                },
            )
            .await?;
        }
        for k in plan.previous.iter().filter(|k| !matched.contains(k)) {
            db::set_change_state(&mut *tx, *k, "removed").await?;
        }
        Ok(())
    }

    /// The current revision's stack.
    pub async fn mr_stack(&self, m: &MergeRequest) -> Result<Stack> {
        let changes = db::changes(&self.pool, m.id).await?;
        let display: HashMap<uuid::Uuid, String> = changes
            .iter()
            .map(|c| (c.id, c.display_id.clone()))
            .collect();
        let versions = db::versions(&self.pool, m.id).await?;
        let revision = (m.revisions > 0).then_some(m.revisions);
        let current: Vec<&db::Version> = versions
            .iter()
            .filter(|v| Some(v.revision_seq) == revision)
            .collect();
        let mut earlier: HashMap<uuid::Uuid, usize> = HashMap::new();
        for v in versions.iter().filter(|v| Some(v.revision_seq) != revision) {
            if let Some(k) = v.change_id {
                *earlier.entry(k).or_default() += 1;
            }
        }
        // "Also in !m": the aliases bound to each change, looked up once.
        let aliases = db::aliases(&self.pool, m.id).await?;
        let (kinds, values): (Vec<String>, Vec<String>) = aliases
            .iter()
            .map(|a| (a.kind.clone(), a.value.clone()))
            .unzip();
        let others = db::also_in(&self.pool, m.id, &kinds, &values).await?;
        let mut also: HashMap<uuid::Uuid, Vec<i32>> = HashMap::new();
        for (kind, value, n) in others {
            if let Some(a) = aliases.iter().find(|a| a.kind == kind && a.value == value) {
                let e = also.entry(a.change_id).or_default();
                if !e.contains(&n) {
                    e.push(n);
                }
            }
        }
        let in_current: HashSet<uuid::Uuid> = current.iter().filter_map(|v| v.change_id).collect();
        Ok(Stack {
            revision,
            entries: current
                .iter()
                .map(|v| StackEntry {
                    position: v.position,
                    commit_oid: v.commit_oid.clone(),
                    display_id: v.change_id.and_then(|k| display.get(&k).cloned()),
                    binding: v.state.clone(),
                    reason: v.reason.clone(),
                    candidates: v
                        .candidates
                        .iter()
                        .filter_map(|k| display.get(k).cloned())
                        .collect(),
                    conflicted: v.conflicted,
                    earlier_versions: v
                        .change_id
                        .and_then(|k| earlier.get(&k).copied())
                        .unwrap_or(0),
                    also_in: v
                        .change_id
                        .and_then(|k| also.get(&k).cloned())
                        .unwrap_or_default(),
                })
                .collect(),
            removed: changes
                .iter()
                .filter(|c| !in_current.contains(&c.id) && earlier.contains_key(&c.id))
                .map(|c| c.display_id.clone())
                .collect(),
        })
    }

    /// Per revision (by number): what changed since the previous one.
    /// Revisions without versions (reviewed as a whole) have no entry.
    pub async fn revision_summaries(
        &self,
        m: &MergeRequest,
    ) -> Result<HashMap<i32, RevisionSummary>> {
        let versions = db::versions(&self.pool, m.id).await?;
        let revisions = slath_db::merge_requests::revisions(&self.pool, m.id).await?;
        let mut out = HashMap::new();
        let mut prev: Option<(i32, HashMap<uuid::Uuid, Option<String>>)> = None;
        for r in &revisions {
            let here: Vec<&db::Version> = versions
                .iter()
                .filter(|v| v.revision_seq == r.seq)
                .collect();
            if here.is_empty() {
                continue;
            }
            let mut sum = RevisionSummary::default();
            let patches: HashMap<uuid::Uuid, Option<String>> = here
                .iter()
                .filter_map(|v| Some((v.change_id?, v.patch_id.clone())))
                .collect();
            if let Some((seq, before)) = &prev {
                let base = |s: i32| {
                    revisions
                        .iter()
                        .find(|x| x.seq == s)
                        .and_then(|x| x.merge_base_oid.clone())
                };
                sum.rebased = base(*seq) != r.merge_base_oid;
                for v in &here {
                    match v.change_id {
                        None => sum.unresolved += 1,
                        Some(k) => match before.get(&k) {
                            None => sum.added += 1,
                            Some(p) if p.is_some() && *p == v.patch_id => sum.unchanged += 1,
                            Some(_) => sum.modified += 1,
                        },
                    }
                }
                sum.removed = before.keys().filter(|k| !patches.contains_key(k)).count();
                out.insert(r.seq, sum);
            }
            prev = Some((r.seq, patches));
        }
        Ok(out)
    }

    /// The push feedback of one merge request: records the revision for
    /// the pushed head `new` within the time budget and lists what happened
    /// to each change. Past the budget, or on errors, only the link.
    pub(crate) async fn push_feedback(
        &self,
        id: MergeRequestId,
        pusher: Option<UserId>,
        new: &str,
        label: &str,
        url: &str,
    ) -> Vec<String> {
        let link = vec![format!("slath: merge request {label}: {url}")];
        let svc = self.clone();
        // Spawned, so that a sync past the budget still finishes.
        let task = tokio::spawn(async move { svc.sync_mr(id, pusher).await });
        let budget = self.config.limits.push_feedback_timeout.get();
        match tokio::time::timeout(budget, task).await {
            Ok(Ok(Ok(_))) => {}
            Ok(Ok(Err(e))) => {
                tracing::warn!(error = %e, mr = %id, "recording a revision during the push failed");
                return link;
            }
            _ => return link,
        }
        let Ok(Some(m)) = slath_db::merge_requests::get_by_id(&self.pool, id).await else {
            return link;
        };
        if m.head_oid.as_deref() != Some(new) {
            return link;
        }
        match self.feedback_lines(&m, label, url).await {
            Ok(lines) => lines,
            Err(e) => {
                tracing::warn!(error = %e, mr = %id, "push feedback failed");
                link
            }
        }
    }

    async fn feedback_lines(
        &self,
        m: &MergeRequest,
        label: &str,
        url: &str,
    ) -> Result<Vec<String>> {
        let seq = m.revisions;
        let mut out = vec![format!(
            "slath: merge request {label}, revision {seq}: {url}"
        )];
        let revisions = slath_db::merge_requests::revisions(&self.pool, m.id).await?;
        let Some(rev) = revisions.iter().find(|r| r.seq == seq) else {
            return Ok(out);
        };
        if rev.mode == "whole-only" {
            let why = match rev.mode_reason.as_deref() {
                Some("unrelated-histories") => "the branches have no common history",
                Some("empty") => "it has no commits of its own",
                Some("too-many-commits") => "it has too many commits",
                Some("merge-commits") => "it contains merge commits",
                _ => "of its history",
            };
            out.push(format!("slath:   reviewed as a whole: {why}"));
            return Ok(out);
        }
        let versions = db::versions(&self.pool, m.id).await?;
        let display: HashMap<uuid::Uuid, String> = db::changes(&self.pool, m.id)
            .await?
            .into_iter()
            .map(|c| (c.id, c.display_id))
            .collect();
        let here: Vec<&db::Version> = versions.iter().filter(|v| v.revision_seq == seq).collect();
        let prev_seq = versions
            .iter()
            .map(|v| v.revision_seq)
            .filter(|s| *s < seq)
            .max();
        let before: HashMap<uuid::Uuid, Option<&str>> = versions
            .iter()
            .filter(|v| Some(v.revision_seq) == prev_seq)
            .filter_map(|v| Some((v.change_id?, v.patch_id.as_deref())))
            .collect();
        // Subjects of the revision's commits.
        let head = ObjectId::parse(&rev.head_oid).map_err(ServiceError::internal)?;
        let base = rev
            .merge_base_oid
            .as_deref()
            .and_then(|b| ObjectId::parse(b).ok());
        let subjects: HashMap<String, String> = slath_git::browse::log_range(
            &self.runner,
            &self.repo_path(m.repo_id),
            base.as_ref(),
            &head,
            self.config.limits.review_max_commits,
        )
        .await
        .map_err(ServiceError::internal)?
        .into_iter()
        .map(|c| (c.oid.as_str().to_owned(), c.subject))
        .collect();
        let subject = |oid: &str| -> String {
            let s: String = subjects
                .get(oid)
                .map(String::as_str)
                .unwrap_or("")
                .chars()
                .filter(|c| !c.is_control())
                .collect();
            if s.chars().count() > 60 {
                format!("{}…", s.chars().take(59).collect::<String>())
            } else {
                s
            }
        };
        let version_of = |k: uuid::Uuid| {
            versions
                .iter()
                .filter(|v| v.change_id == Some(k) && v.revision_seq <= seq)
                .count()
        };
        let mut rows: Vec<(String, String, String)> = Vec::new();
        for v in &here {
            let (id, outcome) = match v.change_id {
                None => (
                    "(unresolved)".to_owned(),
                    if v.reason.as_deref() == Some("duplicate-id") {
                        format!(
                            "unresolved: another commit has the same ID; choose at {url}/commits"
                        )
                    } else {
                        format!(
                            "unresolved: its IDs name {} changes; choose at {url}/commits",
                            v.candidates.len()
                        )
                    },
                ),
                Some(k) => {
                    let id = display
                        .get(&k)
                        .map_or("", |d| d.get(..8).unwrap_or(d))
                        .to_owned();
                    let outcome = match (v.state.as_str(), before.get(&k)) {
                        ("inferred", _) => format!("inferred (version {})", version_of(k)),
                        ("new", _) | (_, None) => "new".to_owned(),
                        (_, Some(p)) if p.is_some() && *p == v.patch_id.as_deref() => {
                            "unchanged".to_owned()
                        }
                        _ => format!("updated (version {})", version_of(k)),
                    };
                    (id, outcome)
                }
            };
            rows.push((id, outcome, subject(&v.commit_oid)));
        }
        let width = rows
            .iter()
            .map(|r| r.1.chars().count())
            .filter(|w| *w < 30)
            .max()
            .unwrap_or(0);
        for (id, outcome, subj) in rows {
            let line = if outcome.starts_with("unresolved") {
                format!("slath:   {id} {subj}: {outcome}")
            } else {
                format!("slath:   {id} {outcome:<width$} {subj}")
            };
            out.push(line.trim_end().to_owned());
        }
        let current: HashSet<uuid::Uuid> = here.iter().filter_map(|v| v.change_id).collect();
        let mut removed: Vec<&String> = before
            .keys()
            .filter(|k| !current.contains(k))
            .filter_map(|k| display.get(k))
            .collect();
        removed.sort();
        for d in removed {
            out.push(format!("slath:   {} removed", d.get(..8).unwrap_or(d)));
        }
        Ok(out)
    }

    /// Whether the subject may rebind commits of this merge request: its
    /// author, or anyone who can push its source.
    #[must_use]
    pub fn can_rebind(subject: &Subject, a: &RepoAccess, m: &MergeRequest) -> bool {
        let author = matches!(subject, Subject::User { id, .. } if m.author_id == Some(*id));
        (author && a.can(subject, Action::MrEditOwn)) || a.can(subject, Action::RepoPush)
    }

    /// Rebinds the commit at `position` of the current revision: resolves
    /// an unresolved commit, splits a continued change into a new one, or
    /// joins a new change to a removed one. Audited, and on the timeline.
    pub async fn rebind_change(
        &self,
        subject: &Subject,
        (ns, repo, number): (&str, &str, i32),
        position: i32,
        to: &Rebind,
    ) -> Result<()> {
        let (a, m) = self.mr(subject, ns, repo, number).await?;
        if !Self::can_rebind(subject, &a, &m) {
            return Err(ServiceError::Forbidden);
        }
        if m.state != "open" {
            return Err(ServiceError::Conflict("merge request isn't open".into()));
        }
        let user = match subject {
            Subject::User { id, .. } => Some(*id),
            _ => None,
        };
        let versions = db::versions(&self.pool, m.id).await?;
        let v = versions
            .iter()
            .find(|v| v.revision_seq == m.revisions && v.position == position)
            .ok_or(ServiceError::NotFound)?;
        let changes = db::changes(&self.pool, m.id).await?;
        let by_display: HashMap<&str, &db::Change> =
            changes.iter().map(|c| (c.display_id.as_str(), c)).collect();
        let in_current: HashSet<uuid::Uuid> = versions
            .iter()
            .filter(|x| x.revision_seq == m.revisions && x.id != v.id)
            .filter_map(|x| x.change_id)
            .collect();
        let earlier = |k: uuid::Uuid| {
            versions
                .iter()
                .any(|x| x.change_id == Some(k) && x.revision_seq < m.revisions)
        };
        let oid = ObjectId::parse(&v.commit_oid).map_err(ServiceError::internal)?;
        let commit = slath_git::changes::stack(&self.runner, &self.repo_path(m.repo_id), &[oid], 0)
            .await
            .map_err(ServiceError::internal)?
            .pop()
            .ok_or(ServiceError::NotFound)?;
        let aliases = aliases_of(&commit);
        let bad = |msg: &str| ServiceError::invalid("change", "invalid", msg.to_owned());
        let mut tx = self.pool.begin().await.map_err(ServiceError::internal)?;
        let (kind, target_display) = match (v.state.as_str(), to) {
            // Resolve: one of the candidates, or a new change.
            ("unresolved", Rebind::Change(d)) => {
                let c = by_display
                    .get(d.as_str())
                    .ok_or_else(|| bad("Choose one of the candidates."))?;
                if !v.candidates.contains(&c.id) || in_current.contains(&c.id) {
                    return Err(bad("Choose one of the candidates."));
                }
                db::set_version(&mut *tx, v.id, c.id, "confirmed", "resolved").await?;
                db::set_change_state(&mut *tx, c.id, "active").await?;
                self.bind_all(&mut tx, m.id, &aliases, c.id, user).await?;
                ("change_resolved", c.display_id.clone())
            }
            ("unresolved", Rebind::New) => {
                let k = self
                    .new_change(&mut tx, &m, &commit, &aliases, &by_display)
                    .await?;
                db::set_version(&mut *tx, v.id, k.0, "new", "resolved").await?;
                self.bind_all(&mut tx, m.id, &aliases, k.0, user).await?;
                ("change_resolved", k.1)
            }
            // Split: a continued change becomes a new one from here on.
            (_, Rebind::New) => {
                let k = v.change_id.ok_or(ServiceError::NotFound)?;
                if !earlier(k) {
                    return Err(bad("This commit already starts its own change."));
                }
                let new = self
                    .new_change(&mut tx, &m, &commit, &aliases, &by_display)
                    .await?;
                db::set_version(&mut *tx, v.id, new.0, "new", "split").await?;
                self.bind_all(&mut tx, m.id, &aliases, new.0, user).await?;
                db::set_change_state(&mut *tx, k, "removed").await?;
                ("change_split", new.1)
            }
            // Join: a new change continues a removed one.
            (_, Rebind::Change(d)) => {
                let k = v.change_id.ok_or(ServiceError::NotFound)?;
                let target = by_display
                    .get(d.as_str())
                    .ok_or_else(|| bad("Choose a removed change."))?;
                if earlier(k)
                    || target.id == k
                    || in_current.contains(&target.id)
                    || !earlier(target.id)
                {
                    return Err(bad("Only a new change can continue a removed change."));
                }
                db::merge_into(&mut tx, k, target.id).await?;
                db::set_version(&mut *tx, v.id, target.id, "confirmed", "join").await?;
                db::set_change_state(&mut *tx, target.id, "active").await?;
                self.bind_all(&mut tx, m.id, &aliases, target.id, user)
                    .await?;
                ("change_joined", target.display_id.clone())
            }
        };
        slath_db::issues::insert_event(
            &mut *tx,
            a.repo.id,
            "merge_request",
            *m.id.as_uuid(),
            user,
            kind,
            &serde_json::json!({"change": target_display, "commit": v.commit_oid}),
        )
        .await?;
        tx.commit().await.map_err(ServiceError::internal)?;
        self.audit(
            subject,
            "mr.change_rebind",
            crate::audit::Target::new("merge_request", Some(*m.id.as_uuid()))
                .in_repo(*a.repo.id.as_uuid()),
            serde_json::json!({"number": m.number, "commit": v.commit_oid, "change": target_display, "kind": kind}),
        )
        .await;
        Ok(())
    }

    async fn bind_all(
        &self,
        tx: &mut sqlx::PgConnection,
        mr: MergeRequestId,
        aliases: &[Alias],
        change: uuid::Uuid,
        by: Option<UserId>,
    ) -> Result<()> {
        let seq = slath_db::merge_requests::get_by_id(&mut *tx, mr)
            .await?
            .map_or(0, |m| m.revisions);
        for a in aliases {
            db::bind_alias(&mut *tx, mr, (a.kind.as_str(), &a.value), change, seq, by).await?;
        }
        Ok(())
    }

    /// A new change for `commit`, with its display ID, or its predicted
    /// ID if that's taken.
    async fn new_change(
        &self,
        tx: &mut sqlx::PgConnection,
        m: &MergeRequest,
        commit: &StackCommit,
        aliases: &[Alias],
        taken: &HashMap<&str, &db::Change>,
    ) -> Result<(uuid::Uuid, String)> {
        let pred = change_identity::predict(commit.oid.as_str())
            .ok_or_else(|| ServiceError::internal("no display ID"))?;
        let display = change_identity::display_id(aliases, commit.oid.as_str())
            .filter(|d| !taken.contains_key(d.as_str()))
            .unwrap_or(pred);
        if taken.contains_key(display.as_str()) {
            return Err(ServiceError::Conflict("display ID in use".into()));
        }
        Ok((db::insert_change(&mut *tx, m.id, &display).await?, display))
    }
}

/// One version of a change, for the change page.
#[derive(Debug, Clone)]
pub struct VersionInfo {
    /// 1 for the first version.
    pub number: usize,
    pub revision: i32,
    pub commit_oid: String,
    pub parent_oid: Option<String>,
    /// `new`, `confirmed`, `inferred`, or `unresolved`.
    pub binding: String,
    pub reason: Option<String>,
}

/// A change with all its versions, oldest first.
#[derive(Debug, Clone)]
pub struct ChangeInfo {
    pub display_id: String,
    pub state: String,
    pub versions: Vec<VersionInfo>,
}

/// How two versions of a change are compared.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Interdiff {
    /// Same parent: the tree diff is exactly what changed.
    Exact(Vec<u8>, bool),
    /// Different parents, compared as patches (`git range-diff`).
    Patches(Vec<u8>, bool),
    /// Different parents, as a tree diff that includes what the rebase
    /// brought in.
    Tree(Vec<u8>, bool),
}

const MAX_DIFF_BYTES: usize = 64 * 1024 * 1024;

fn oid(s: &str) -> Result<ObjectId> {
    ObjectId::parse(s).map_err(ServiceError::internal)
}

impl Services {
    /// A change of the merge request, by its display ID (in full or its
    /// first 8 characters) or the commit ID of one of its versions.
    pub async fn mr_change(&self, m: &MergeRequest, id: &str) -> Result<ChangeInfo> {
        let changes = db::changes(&self.pool, m.id).await?;
        let versions = db::versions(&self.pool, m.id).await?;
        let by_display: Vec<&db::Change> = changes
            .iter()
            .filter(|c| c.display_id == id || (id.len() >= 8 && c.display_id.starts_with(id)))
            .collect();
        let change = match by_display.as_slice() {
            [one] => Some(*one),
            _ => versions
                .iter()
                .find(|v| v.commit_oid == id)
                .and_then(|v| v.change_id)
                .and_then(|k| changes.iter().find(|c| c.id == k)),
        }
        .ok_or(ServiceError::NotFound)?;
        Ok(ChangeInfo {
            display_id: change.display_id.clone(),
            state: change.state.clone(),
            versions: versions
                .iter()
                .filter(|v| v.change_id == Some(change.id))
                .enumerate()
                .map(|(i, v)| VersionInfo {
                    number: i + 1,
                    revision: v.revision_seq,
                    commit_oid: v.commit_oid.clone(),
                    parent_oid: v.parent_oid.clone(),
                    binding: v.state.clone(),
                    reason: v.reason.clone(),
                })
                .collect(),
        })
    }

    /// The patch of one version: its commit against its parent.
    pub async fn mr_version_diff(
        &self,
        m: &MergeRequest,
        v: &VersionInfo,
        opts: slath_git::browse::DiffOptions,
    ) -> Result<(Vec<u8>, bool)> {
        let parent = v.parent_oid.as_deref().map(oid).transpose()?;
        slath_git::browse::diff(
            &self.runner,
            &self.repo_path(m.repo_id),
            parent.as_ref(),
            &oid(&v.commit_oid)?,
            opts,
            MAX_DIFF_BYTES,
        )
        .await
        .map_err(ServiceError::internal)
    }

    /// Two versions of a change compared (spec: change interdiff). With
    /// the same parent, exact; otherwise as patches, or with `tree` as a
    /// tree diff including the rebase.
    pub async fn mr_version_interdiff(
        &self,
        m: &MergeRequest,
        a: &VersionInfo,
        b: &VersionInfo,
        tree: bool,
        opts: slath_git::browse::DiffOptions,
    ) -> Result<Interdiff> {
        let dir = self.repo_path(m.repo_id);
        let (ca, cb) = (oid(&a.commit_oid)?, oid(&b.commit_oid)?);
        if a.parent_oid == b.parent_oid || tree {
            let (raw, cut) =
                slath_git::browse::diff(&self.runner, &dir, Some(&ca), &cb, opts, MAX_DIFF_BYTES)
                    .await
                    .map_err(ServiceError::internal)?;
            return Ok(if a.parent_oid == b.parent_oid {
                Interdiff::Exact(raw, cut)
            } else {
                Interdiff::Tree(raw, cut)
            });
        }
        let (Some(pa), Some(pb)) = (a.parent_oid.as_deref(), b.parent_oid.as_deref()) else {
            // A root commit has no patch to compare with range-diff.
            let (raw, cut) =
                slath_git::browse::diff(&self.runner, &dir, Some(&ca), &cb, opts, MAX_DIFF_BYTES)
                    .await
                    .map_err(ServiceError::internal)?;
            return Ok(Interdiff::Tree(raw, cut));
        };
        let (out, cut) = slath_git::browse::range_diff(
            &self.runner,
            &dir,
            (&oid(pa)?, &ca),
            (&oid(pb)?, &cb),
            100,
            MAX_DIFF_BYTES,
        )
        .await
        .map_err(ServiceError::internal)?;
        Ok(Interdiff::Patches(out, cut))
    }

    /// Two revisions compared with `git range-diff` (spec: revision
    /// interdiff): heuristic, since it pairs commits by similarity.
    /// `None` if either revision is unknown or has no merge base.
    pub async fn mr_revision_interdiff(
        &self,
        m: &MergeRequest,
        from: i32,
        to: i32,
    ) -> Result<Option<(Vec<u8>, bool)>> {
        let revisions = slath_db::merge_requests::revisions(&self.pool, m.id).await?;
        let find = |seq: i32| revisions.iter().find(|r| r.seq == seq);
        let (Some(a), Some(b)) = (find(from), find(to)) else {
            return Ok(None);
        };
        let (Some(ma), Some(mb)) = (a.merge_base_oid.as_deref(), b.merge_base_oid.as_deref())
        else {
            return Ok(None);
        };
        slath_git::browse::range_diff(
            &self.runner,
            &self.repo_path(m.repo_id),
            (&oid(ma)?, &oid(&a.head_oid)?),
            (&oid(mb)?, &oid(&b.head_oid)?),
            100,
            MAX_DIFF_BYTES,
        )
        .await
        .map(Some)
        .map_err(ServiceError::internal)
    }
}
