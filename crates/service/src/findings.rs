// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Findings as review threads: a status's line annotations become threads
//! on each open merge request revision of the same repository whose head is
//! the annotated commit, and a later status of the same context resolves
//! the ones it no longer reports (spec/07 commit statuses).

use std::collections::{HashMap, HashSet};

use sha2::{Digest, Sha256};
use slath_core::ids::{RepoId, UserId};
use slath_db::mr_findings::{self as db, NewFinding};
use slath_db::mr_threads::Anchor;
use slath_db::statuses::Annotation;
use slath_git::ObjectId;

use crate::Services;
use crate::error::{Result, ServiceError};
use crate::mr_threads::hash_lines;

/// The status a finding comes from.
pub(crate) struct Report<'a> {
    pub repo: RepoId,
    pub commit: &'a ObjectId,
    pub context: &'a str,
    pub status: uuid::Uuid,
    pub reporter: UserId,
}

/// A finding's identity within its context and revision.
fn key(context: &str, a: &Annotation) -> String {
    let mut h = Sha256::new();
    for part in [
        context,
        &a.path,
        &a.line_start.to_string(),
        &a.line_end.to_string(),
        &a.severity,
        &a.category,
        &a.message,
    ] {
        h.update(part.len().to_be_bytes());
        h.update(part.as_bytes());
    }
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

/// A finding ready to become a thread.
struct Prepared<'a> {
    key: String,
    note: &'a Annotation,
    lines: Vec<Vec<u8>>,
    context: serde_json::Value,
}

impl Services {
    /// Projects a just-recorded status onto merge request threads, in the
    /// status's transaction.
    pub(crate) async fn project_findings(
        &self,
        tx: &mut sqlx::PgConnection,
        r: &Report<'_>,
        annotations: &[Annotation],
    ) -> Result<()> {
        let targets = db::targets(&mut *tx, r.repo, r.commit.as_str()).await?;
        if targets.is_empty() {
            return Ok(());
        }
        let prepared = self.prepare(r, annotations).await?;
        let keys: HashSet<&str> = prepared.iter().map(|p| p.key.as_str()).collect();
        for t in targets {
            db::lock(&mut *tx, t.mr).await?;
            let existing: HashMap<String, db::Existing> =
                db::of_context(&mut *tx, t.mr, t.seq, r.context)
                    .await?
                    .into_iter()
                    .map(|e| (e.key.clone(), e))
                    .collect();
            let mut changed = false;
            for p in &prepared {
                match existing.get(&p.key) {
                    Some(e) if e.resolved && e.resolved_by_status => {
                        db::set_resolved_by_status(&mut *tx, e.thread, None).await?;
                        self.finding_event(tx, r, t.mr, e.thread, e.path.as_deref(), false)
                            .await?;
                        changed = true;
                    }
                    Some(_) => {}
                    None => {
                        let a = p.note;
                        let anchor = Anchor {
                            old_oid: t.merge_base_oid.clone(),
                            new_oid: Some(r.commit.as_str().to_owned()),
                            path: Some(a.path.clone()),
                            side: Some("new".to_owned()),
                            line_start: Some(a.line_start),
                            line_end: Some(a.line_end),
                            line_hash: Some(hash_lines(&p.lines)),
                            context: p.context.clone(),
                        };
                        let thread = slath_db::mr_threads::insert_thread(
                            &mut *tx, t.mr, t.seq, &anchor, r.reporter,
                        )
                        .await?;
                        slath_db::mr_threads::insert_comment(
                            &mut *tx, thread, r.reporter, &a.message, false,
                        )
                        .await?;
                        db::insert(
                            &mut *tx,
                            &NewFinding {
                                thread,
                                mr: t.mr,
                                seq: t.seq,
                                context: r.context,
                                key: &p.key,
                                severity: &a.severity,
                                category: &a.category,
                                status: r.status,
                            },
                        )
                        .await?;
                        changed = true;
                    }
                }
            }
            for e in existing.values() {
                if !e.resolved && !keys.contains(e.key.as_str()) {
                    db::set_resolved_by_status(&mut *tx, e.thread, Some((r.status, r.reporter)))
                        .await?;
                    self.finding_event(tx, r, t.mr, e.thread, e.path.as_deref(), true)
                        .await?;
                    changed = true;
                }
            }
            if changed {
                slath_db::merge_requests::touch(&mut *tx, t.mr).await?;
            }
        }
        Ok(())
    }

    /// Each distinct finding with its lines and their surroundings, read
    /// once per file.
    async fn prepare<'a>(
        &self,
        r: &Report<'_>,
        annotations: &'a [Annotation],
    ) -> Result<Vec<Prepared<'a>>> {
        let dir = self.repo_path(r.repo);
        let mut files: HashMap<&str, Vec<Vec<u8>>> = HashMap::new();
        let mut seen = HashSet::new();
        let mut out = Vec::new();
        for a in annotations {
            let key = key(r.context, a);
            if !seen.insert(key.clone()) {
                continue;
            }
            if !files.contains_key(a.path.as_str()) {
                let all =
                    slath_git::linemap::file_lines(&self.runner, &dir, r.commit, a.path.as_bytes())
                        .await
                        .map_err(ServiceError::internal)?
                        .ok_or(ServiceError::NotFound)?;
                files.insert(&a.path, all);
            }
            let all = &files[a.path.as_str()];
            let (Ok(s), Ok(e)) = (usize::try_from(a.line_start), usize::try_from(a.line_end))
            else {
                return Err(ServiceError::internal("annotation lines"));
            };
            if s == 0 || e > all.len() {
                return Err(ServiceError::internal("annotation lines"));
            }
            let text = |v: &[Vec<u8>]| -> Vec<String> {
                v.iter()
                    .map(|l| String::from_utf8_lossy(l).into_owned())
                    .collect()
            };
            let lines = all[s - 1..e].to_vec();
            let before = &all[s.saturating_sub(4)..s - 1];
            let after = &all[e..(e + 3).min(all.len())];
            out.push(Prepared {
                key,
                note: a,
                context: serde_json::json!({
                    "before": text(before),
                    "lines": text(&lines),
                    "after": text(after),
                }),
                lines,
            });
        }
        Ok(out)
    }

    async fn finding_event(
        &self,
        tx: &mut sqlx::PgConnection,
        r: &Report<'_>,
        mr: slath_core::ids::MergeRequestId,
        thread: uuid::Uuid,
        path: Option<&str>,
        resolved: bool,
    ) -> Result<()> {
        slath_db::issues::insert_event(
            &mut *tx,
            r.repo,
            "merge_request",
            *mr.as_uuid(),
            Some(r.reporter),
            if resolved {
                "thread_resolved"
            } else {
                "thread_reopened"
            },
            &serde_json::json!({"thread": thread, "path": path, "finding": r.context}),
        )
        .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn note(message: &str) -> Annotation {
        Annotation {
            path: "a".into(),
            line_start: 1,
            line_end: 2,
            severity: "warning".into(),
            category: "other".into(),
            message: message.into(),
        }
    }

    #[test]
    fn keys() {
        assert_eq!(key("lint", &note("x")), key("lint", &note("x")));
        assert_ne!(key("lint", &note("x")), key("test", &note("x")));
        assert_ne!(key("lint", &note("x")), key("lint", &note("y")));
        let mut moved = note("x");
        moved.line_end = 3;
        assert_ne!(key("lint", &note("x")), key("lint", &moved));
        // Parts are length-prefixed, so boundaries can't shift.
        let mut a = note("x");
        a.path = "ab".into();
        a.severity = "c".into();
        let mut b = note("x");
        b.path = "a".into();
        b.severity = "bc".into();
        assert_ne!(key("lint", &a), key("lint", &b));
    }
}
