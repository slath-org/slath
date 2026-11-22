// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Review threads (spec: comments, batched reviews, suggestions): threads
//! anchored to lines of an immutable diff, comments that stay pending until
//! the review is submitted, resolving, and where a thread is in the current
//! revision: carried forward only when its lines are unchanged, otherwise
//! outdated (REVIEW-02).

use sha2::{Digest, Sha256};
use slath_core::authz::{Action, Subject};
use slath_core::ids::UserId;
use slath_db::merge_requests::MergeRequest;
use slath_db::mr_threads::{self as db, Anchor, Thread, ThreadComment};
use slath_git::ObjectId;

use crate::Services;
use crate::error::{Result, ServiceError};
use crate::notifications::MrActivity;
use crate::repos::RepoAccess;

/// What starting a thread takes. Without `path`, a thread on the whole
/// merge request.
#[derive(Debug, Clone, Default)]
pub struct NewThread {
    pub body: String,
    pub path: Option<String>,
    /// `old` or `new` (default).
    pub side: Option<String>,
    pub line_start: Option<i32>,
    pub line_end: Option<i32>,
    /// The revision whose diff it's on (default: the current one).
    pub revision: Option<i32>,
    /// A version's commit, for a thread on one change's diff.
    pub commit: Option<String>,
    /// Part of the author's pending review.
    pub pending: bool,
}

/// Where a thread shows in the current revision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Position {
    /// On the merge request as a whole.
    General,
    /// On these lines of the current diff; `from` is the revision it was
    /// made on when that's an earlier one.
    Current {
        path: String,
        side: String,
        line_start: i32,
        line_end: i32,
        from: Option<i32>,
    },
    /// Its lines changed: shown in the conversation and on its own
    /// revision, never on a guessed line.
    Outdated,
}

/// A thread with the comments the viewer may see and its position.
#[derive(Debug, Clone)]
pub struct ThreadView {
    pub thread: Thread,
    pub comments: Vec<ThreadComment>,
    pub position: Position,
}

pub(crate) fn hash_lines(lines: &[Vec<u8>]) -> String {
    let mut h = Sha256::new();
    for (i, l) in lines.iter().enumerate() {
        if i > 0 {
            h.update(b"\n");
        }
        h.update(l);
    }
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

fn is_bot(subject: &Subject) -> bool {
    matches!(
        subject,
        Subject::User {
            kind: slath_core::authz::UserKind::Bot,
            ..
        }
    )
}

fn me(subject: &Subject) -> Result<UserId> {
    match subject {
        Subject::User { id, .. } => Ok(*id),
        _ => Err(ServiceError::Unauthenticated),
    }
}

fn oid(s: &str) -> Result<ObjectId> {
    ObjectId::parse(s).map_err(ServiceError::internal)
}

impl Services {
    /// The diff of a revision: its merge base (or the empty tree's absence)
    /// and head.
    pub(crate) async fn revision_diff(
        &self,
        m: &MergeRequest,
        seq: i32,
    ) -> Result<(Option<String>, String)> {
        let revs = slath_db::merge_requests::revisions(&self.pool, m.id).await?;
        let r = revs
            .iter()
            .find(|r| r.seq == seq)
            .ok_or(ServiceError::NotFound)?;
        Ok((r.merge_base_oid.clone(), r.head_oid.clone()))
    }

    /// Starts a thread (and its first comment).
    pub async fn create_thread(
        &self,
        subject: &Subject,
        (ns, repo, number): (&str, &str, i32),
        t: &NewThread,
    ) -> Result<ThreadView> {
        let (a, m) = self.mr(subject, ns, repo, number).await?;
        if !a.can(subject, Action::MrComment) {
            return Err(ServiceError::Forbidden);
        }
        self.require_confirmed(subject).await?;
        let author = me(subject)?;
        if t.body.trim().is_empty() {
            return Err(ServiceError::invalid(
                "body",
                "required",
                "Write a comment.",
            ));
        }
        self.check_body("body", &t.body)?;
        let seq = t.revision.unwrap_or(m.revisions);
        if seq < 1 || seq > m.revisions {
            return Err(ServiceError::invalid(
                "revision",
                "invalid",
                "Choose an existing revision.",
            ));
        }
        let anchor = match &t.path {
            None => Anchor::default(),
            Some(path) => self.anchor(&m, seq, t, path).await?,
        };
        let origin = self.origin_of(subject, &a.repo).await?;
        let mut tx = self.pool.begin().await.map_err(ServiceError::internal)?;
        let id = db::insert_thread(&mut *tx, m.id, seq, &anchor, author).await?;
        let comment = db::insert_comment(
            &mut *tx,
            id,
            author,
            t.body.trim_end(),
            (t.pending, false),
            origin,
        )
        .await?;
        slath_db::merge_requests::touch(&mut *tx, m.id).await?;
        tx.commit().await.map_err(ServiceError::internal)?;
        if !t.pending {
            self.notify_mr(Some(author), &m, MrActivity::ThreadComment(comment))
                .await;
        }
        self.mr_threads(subject, &a, &m)
            .await?
            .into_iter()
            .find(|v| v.thread.id == id)
            .ok_or(ServiceError::NotFound)
    }

    async fn anchor(
        &self,
        m: &MergeRequest,
        seq: i32,
        t: &NewThread,
        path: &str,
    ) -> Result<Anchor> {
        let bad = |field: &'static str, msg: &str| {
            ServiceError::invalid(field, "invalid", msg.to_owned())
        };
        let (old, new) = match &t.commit {
            Some(c) => {
                let v = slath_db::mr_changes::versions(&self.pool, m.id)
                    .await?
                    .into_iter()
                    .find(|v| v.revision_seq == seq && v.commit_oid == *c)
                    .ok_or_else(|| bad("commit", "This commit isn't part of that revision."))?;
                (v.parent_oid, v.commit_oid)
            }
            None => self.revision_diff(m, seq).await?,
        };
        let side = t.side.clone().unwrap_or_else(|| "new".to_owned());
        let at = match side.as_str() {
            "new" => new.clone(),
            "old" => old
                .clone()
                .ok_or_else(|| bad("side", "There's no old side here."))?,
            _ => return Err(bad("side", "Use old or new.")),
        };
        let start = t
            .line_start
            .ok_or_else(|| bad("line_start", "Choose a line."))?;
        let end = t.line_end.unwrap_or(start);
        let (Ok(s), Ok(e)) = (u32::try_from(start), u32::try_from(end)) else {
            return Err(bad("line_start", "Choose existing lines."));
        };
        if s == 0 || e < s || e - s > 500 {
            return Err(bad("line_start", "Choose existing lines."));
        }
        let dir = self.repo_path(m.repo_id);
        let lines =
            slath_git::linemap::read_lines(&self.runner, &dir, &oid(&at)?, path.as_bytes(), (s, e))
                .await
                .map_err(ServiceError::internal)?
                .ok_or_else(|| bad("line_start", "These lines don't exist in that file."))?;
        // Up to three lines on each side, for showing an outdated thread.
        let before = slath_git::linemap::read_lines(
            &self.runner,
            &dir,
            &oid(&at)?,
            path.as_bytes(),
            (s.saturating_sub(3).max(1), s.saturating_sub(1).max(1)),
        )
        .await
        .map_err(ServiceError::internal)?
        .filter(|_| s > 1)
        .unwrap_or_default();
        let mut after = Vec::new();
        for k in 1..=3 {
            match slath_git::linemap::read_lines(
                &self.runner,
                &dir,
                &oid(&at)?,
                path.as_bytes(),
                (e + k, e + k),
            )
            .await
            .map_err(ServiceError::internal)?
            {
                Some(mut l) => after.append(&mut l),
                None => break,
            }
        }
        let text = |v: &[Vec<u8>]| -> Vec<String> {
            v.iter()
                .map(|l| String::from_utf8_lossy(l).into_owned())
                .collect()
        };
        Ok(Anchor {
            old_oid: old,
            new_oid: Some(new),
            path: Some(path.to_owned()),
            side: Some(side),
            line_start: Some(start),
            line_end: Some(end),
            line_hash: Some(hash_lines(&lines)),
            context: serde_json::json!({"before": text(&before), "lines": text(&lines), "after": text(&after)}),
        })
    }

    /// Adds a comment to a thread. With `resolve`, the reply also resolves
    /// the thread (for whoever may resolve it): a reply sent now resolves
    /// it in the same transaction, a pending one when its review is
    /// submitted.
    pub async fn reply(
        &self,
        subject: &Subject,
        (ns, repo, number): (&str, &str, i32),
        thread: uuid::Uuid,
        body: &str,
        (pending, resolve): (bool, bool),
    ) -> Result<ThreadComment> {
        let (a, m) = self.mr(subject, ns, repo, number).await?;
        if !a.can(subject, Action::MrComment) {
            return Err(ServiceError::Forbidden);
        }
        self.require_confirmed(subject).await?;
        let author = me(subject)?;
        let t = self.visible_thread(subject, &a, &m, thread).await?;
        if body.trim().is_empty() {
            return Err(ServiceError::invalid(
                "body",
                "required",
                "Write a comment.",
            ));
        }
        self.check_body("body", body)?;
        // Resolving needs the same right as the Resolve button; an
        // already resolved thread stays as it is.
        let resolve = resolve && t.thread.resolved_at.is_none();
        if resolve {
            Self::may_resolve(subject, &a, &m, &t)?;
        }
        let origin = self.origin_of(subject, &a.repo).await?;
        let mut tx = self.pool.begin().await.map_err(ServiceError::internal)?;
        let id = db::insert_comment(
            &mut *tx,
            thread,
            author,
            body.trim_end(),
            (pending, resolve),
            origin,
        )
        .await?;
        // An answer by anyone but whoever marked it addressed reopens the
        // question (pending ones when their review is submitted).
        if !pending {
            db::clear_addressed_unless(&mut *tx, thread, author).await?;
            if resolve {
                Self::resolve_in(&mut tx, &a, &m, &t, author).await?;
            }
        }
        slath_db::merge_requests::touch(&mut *tx, m.id).await?;
        tx.commit().await.map_err(ServiceError::internal)?;
        if !pending {
            self.notify_mr(Some(author), &m, MrActivity::ThreadComment(id))
                .await;
            if resolve {
                self.queue_auto_merge(m.id).await;
            }
        }
        db::comments(&self.pool, m.id)
            .await?
            .into_iter()
            .find(|c| c.id == id)
            .ok_or(ServiceError::NotFound)
    }

    /// Whether `subject` may resolve `t`: as for the Resolve button, and a
    /// bot only threads bots started.
    pub(crate) fn may_resolve(
        subject: &Subject,
        a: &RepoAccess,
        m: &MergeRequest,
        t: &ThreadView,
    ) -> Result<UserId> {
        let user = Self::may_settle(subject, a, m, t)?;
        if is_bot(subject) && !t.thread.author_bot {
            return Err(ServiceError::Conflict(
                "A person started this thread, so a person resolves it. Mark it as addressed instead."
                    .to_owned(),
            ));
        }
        Ok(user)
    }

    /// Resolves `t` for `user` with its timeline event, inside `tx`.
    pub(crate) async fn resolve_in(
        tx: &mut sqlx::PgConnection,
        a: &RepoAccess,
        m: &MergeRequest,
        t: &ThreadView,
        user: UserId,
    ) -> Result<()> {
        db::set_resolved(&mut *tx, t.thread.id, Some(user)).await?;
        slath_db::issues::insert_event(
            &mut *tx,
            a.repo.id,
            "merge_request",
            *m.id.as_uuid(),
            Some(user),
            "thread_resolved",
            &serde_json::json!({"thread": t.thread.id, "path": t.thread.anchor.path}),
        )
        .await?;
        Ok(())
    }

    async fn visible_thread(
        &self,
        subject: &Subject,
        a: &RepoAccess,
        m: &MergeRequest,
        thread: uuid::Uuid,
    ) -> Result<ThreadView> {
        self.mr_threads(subject, a, m)
            .await?
            .into_iter()
            .find(|v| v.thread.id == thread)
            .ok_or(ServiceError::NotFound)
    }

    /// Whether `subject` may resolve, reopen, or mark a thread addressed:
    /// the merge request's author, the thread's author, or someone who can
    /// push.
    fn may_settle(
        subject: &Subject,
        a: &RepoAccess,
        m: &MergeRequest,
        t: &ThreadView,
    ) -> Result<UserId> {
        let user = me(subject)?;
        let allowed = m.author_id == Some(user)
            || t.thread.author_id == Some(user)
            || a.can(subject, Action::RepoPush);
        if !allowed || !a.can(subject, Action::MrComment) {
            return Err(ServiceError::Forbidden);
        }
        Ok(user)
    }

    /// Resolves or reopens a thread: the merge request's author, the
    /// thread's author, or someone who can push. A bot settles only threads
    /// bots started; a person's thread needs a person.
    pub async fn set_thread_resolved(
        &self,
        subject: &Subject,
        (ns, repo, number): (&str, &str, i32),
        thread: uuid::Uuid,
        resolved: bool,
    ) -> Result<()> {
        let (a, m) = self.mr(subject, ns, repo, number).await?;
        let t = self.visible_thread(subject, &a, &m, thread).await?;
        let user = Self::may_resolve(subject, &a, &m, &t)?;
        if t.thread.resolved_at.is_some() == resolved {
            return Ok(());
        }
        let mut tx = self.pool.begin().await.map_err(ServiceError::internal)?;
        db::set_resolved(&mut *tx, thread, resolved.then_some(user)).await?;
        slath_db::issues::insert_event(
            &mut *tx,
            a.repo.id,
            "merge_request",
            *m.id.as_uuid(),
            Some(user),
            if resolved {
                "thread_resolved"
            } else {
                "thread_reopened"
            },
            &serde_json::json!({"thread": thread, "path": t.thread.anchor.path}),
        )
        .await?;
        tx.commit().await.map_err(ServiceError::internal)?;
        if resolved {
            self.queue_auto_merge(m.id).await;
        }
        Ok(())
    }

    /// Marks a thread addressed (or clears that), for whoever may resolve
    /// it; bots use it for threads people started. It satisfies nothing:
    /// the thread stays open until someone resolves it.
    pub async fn set_thread_addressed(
        &self,
        subject: &Subject,
        (ns, repo, number): (&str, &str, i32),
        thread: uuid::Uuid,
        addressed: bool,
    ) -> Result<()> {
        let (a, m) = self.mr(subject, ns, repo, number).await?;
        let t = self.visible_thread(subject, &a, &m, thread).await?;
        let user = Self::may_settle(subject, &a, &m, &t)?;
        if addressed && t.thread.resolved_at.is_some() {
            return Err(ServiceError::Conflict(
                "This thread is resolved already.".to_owned(),
            ));
        }
        let mut tx = self.pool.begin().await.map_err(ServiceError::internal)?;
        let changed = db::set_addressed(&mut *tx, thread, addressed.then_some(user)).await?;
        if changed && addressed {
            slath_db::issues::insert_event(
                &mut *tx,
                a.repo.id,
                "merge_request",
                *m.id.as_uuid(),
                Some(user),
                "thread_addressed",
                &serde_json::json!({"thread": thread, "path": t.thread.anchor.path}),
            )
            .await?;
        }
        tx.commit().await.map_err(ServiceError::internal)?;
        Ok(())
    }

    /// The threads the viewer may see (published comments, and their own
    /// pending ones), with their position in the current revision.
    pub async fn mr_threads(
        &self,
        subject: &Subject,
        _a: &RepoAccess,
        m: &MergeRequest,
    ) -> Result<Vec<ThreadView>> {
        let viewer = match subject {
            Subject::User { id, .. } => Some(*id),
            _ => None,
        };
        let comments = db::comments(&self.pool, m.id).await?;
        let mut out = Vec::new();
        let current = if m.revisions > 0 {
            Some(self.revision_diff(m, m.revisions).await?)
        } else {
            None
        };
        for t in db::threads(&self.pool, m.id).await? {
            let visible: Vec<ThreadComment> = comments
                .iter()
                .filter(|c| {
                    c.thread_id == t.id
                        && c.deleted_at.is_none()
                        && (!c.pending || (viewer.is_some() && c.author_id == viewer))
                })
                .cloned()
                .collect();
            if visible.is_empty() {
                continue;
            }
            let position = match &current {
                // Findings stay on their revision (a later one gets its own).
                Some(_) if t.finding.is_some() && t.revision_seq != m.revisions => {
                    Position::Outdated
                }
                Some(cur) => self.position(m, &t, cur).await?,
                None => Position::Outdated,
            };
            out.push(ThreadView {
                thread: t,
                comments: visible,
                position,
            });
        }
        Ok(out)
    }

    /// Where `t` is in the current diff (`mb..head`).
    async fn position(
        &self,
        m: &MergeRequest,
        t: &Thread,
        (mb, head): &(Option<String>, String),
    ) -> Result<Position> {
        let a = &t.anchor;
        let (Some(path), Some(side), Some(start), Some(end), Some(hash)) =
            (&a.path, &a.side, a.line_start, a.line_end, &a.line_hash)
        else {
            return Ok(Position::General);
        };
        let (from_oid, to_oid) = match side.as_str() {
            "new" => (a.new_oid.clone(), Some(head.clone())),
            _ => (a.old_oid.clone(), mb.clone()),
        };
        let (Some(from_oid), Some(to_oid)) = (from_oid, to_oid) else {
            return Ok(Position::Outdated);
        };
        let (Ok(s), Ok(e)) = (u32::try_from(start), u32::try_from(end)) else {
            return Ok(Position::Outdated);
        };
        let dir = self.repo_path(m.repo_id);
        let (from, to) = (oid(&from_oid)?, oid(&to_oid)?);
        let Some((new_path, ns, ne)) =
            slath_git::linemap::carry(&self.runner, &dir, (&from, &to), path.as_bytes(), (s, e))
                .await
                .map_err(ServiceError::internal)?
        else {
            return Ok(Position::Outdated);
        };
        let lines = slath_git::linemap::read_lines(&self.runner, &dir, &to, &new_path, (ns, ne))
            .await
            .map_err(ServiceError::internal)?;
        if lines.is_none_or(|l| hash_lines(&l) != *hash) {
            return Ok(Position::Outdated);
        }
        Ok(Position::Current {
            path: String::from_utf8_lossy(&new_path).into_owned(),
            side: side.clone(),
            line_start: i32::try_from(ns).map_err(ServiceError::internal)?,
            line_end: i32::try_from(ne).map_err(ServiceError::internal)?,
            from: (t.revision_seq != m.revisions).then_some(t.revision_seq),
        })
    }

    /// Published threads that aren't resolved, findings aside (they never
    /// count as requirements).
    pub async fn unresolved_threads(&self, m: &MergeRequest) -> Result<usize> {
        Ok(self.unresolved_threads_upto(m, None).await?.0)
    }

    /// Unresolved threads for landing the changes `prefix` (display IDs;
    /// `None`: the whole merge request): `(blocking, on later changes)`.
    /// A thread belongs to one change when it was made on that change's
    /// own diff (a version's parent to its commit); only those on changes
    /// outside the prefix don't block. General threads and threads on the
    /// whole diff block, as they may be about any change.
    pub(crate) async fn unresolved_threads_upto(
        &self,
        m: &MergeRequest,
        prefix: Option<&[String]>,
    ) -> Result<(usize, usize)> {
        let comments = db::comments(&self.pool, m.id).await?;
        let open: Vec<db::Thread> = db::threads(&self.pool, m.id)
            .await?
            .into_iter()
            .filter(|t| {
                t.resolved_at.is_none()
                    && t.finding.is_none()
                    && comments
                        .iter()
                        .any(|c| c.thread_id == t.id && !c.pending && c.deleted_at.is_none())
            })
            .collect();
        let Some(prefix) = prefix else {
            return Ok((open.len(), 0));
        };
        let display: std::collections::HashMap<uuid::Uuid, String> =
            slath_db::mr_changes::changes(&self.pool, m.id)
                .await?
                .into_iter()
                .map(|c| (c.id, c.display_id))
                .collect();
        // Each version's own diff, and its change.
        let own: std::collections::HashMap<(String, String), &String> =
            slath_db::mr_changes::versions(&self.pool, m.id)
                .await?
                .into_iter()
                .filter_map(|v| {
                    let d = display.get(&v.change_id?)?;
                    Some(((v.parent_oid?, v.commit_oid), d))
                })
                .collect();
        let later = open
            .iter()
            .filter(|t| {
                let (Some(o), Some(n)) = (&t.anchor.old_oid, &t.anchor.new_oid) else {
                    return false;
                };
                own.get(&(o.clone(), n.clone()))
                    .is_some_and(|d| !prefix.contains(d))
            })
            .count();
        Ok((open.len() - later, later))
    }

    /// How many of the viewer's comments wait for their review.
    pub async fn pending_comments(&self, subject: &Subject, m: &MergeRequest) -> Result<usize> {
        let Subject::User { id, .. } = subject else {
            return Ok(0);
        };
        Ok(db::comments(&self.pool, m.id)
            .await?
            .iter()
            .filter(|c| c.pending && c.author_id == Some(*id) && c.deleted_at.is_none())
            .count())
    }
}

/// The blob of `path` at `commit`, or 40 zeros if the file isn't there.
async fn blob_at(s: &Services, m: &MergeRequest, commit: &str, path: &str) -> Result<String> {
    let out = s
        .runner
        .repo(&s.repo_path(m.repo_id))
        .args(["rev-parse", "--verify", "--quiet"])
        .arg(format!("{commit}:{path}"))
        .output()
        .await
        .map_err(ServiceError::internal)?;
    let text = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    Ok(if out.status.success() && text.len() == 40 {
        text
    } else {
        "0".repeat(40)
    })
}

/// What a viewed mark is for.
#[derive(Debug, Clone, Copy)]
pub enum Viewed<'a> {
    /// A whole file, with the keys of its hunks in the diff shown.
    File { viewed: bool, hunks: &'a [String] },
    /// One hunk, by its key.
    Hunk { key: &'a String, viewed: bool },
}

impl Services {
    /// The files the viewer marked viewed that are still as they saw them
    /// at the current head (a changed file counts as not viewed).
    pub async fn viewed_files(
        &self,
        subject: &Subject,
        m: &MergeRequest,
    ) -> Result<std::collections::HashSet<String>> {
        let mut out = std::collections::HashSet::new();
        let (Subject::User { id, .. }, Some(head)) = (subject, m.head_oid.as_deref()) else {
            return Ok(out);
        };
        for (path, blob) in db::viewed(&self.pool, m.id, *id).await? {
            if blob_at(self, m, head, &path).await? == blob {
                out.insert(path);
            }
        }
        Ok(out)
    }

    /// The hunks the viewer marked viewed, as (path, key).
    pub async fn viewed_hunks(
        &self,
        subject: &Subject,
        m: &MergeRequest,
    ) -> Result<std::collections::HashSet<(String, String)>> {
        let Subject::User { id, .. } = subject else {
            return Ok(std::collections::HashSet::new());
        };
        Ok(db::viewed_hunks(&self.pool, m.id, *id)
            .await?
            .into_iter()
            .collect())
    }

    /// Marks a file of the current head, or one hunk of it, viewed or not
    /// viewed (spec: viewed hunks). A file's mark also marks `hunks` (the
    /// keys of its hunks in the diff shown); unmarking a hunk also clears
    /// its file's mark.
    pub async fn set_viewed(
        &self,
        subject: &Subject,
        (ns, repo, number): (&str, &str, i32),
        path: &str,
        mark: Viewed<'_>,
    ) -> Result<()> {
        let (_, m) = self.mr(subject, ns, repo, number).await?;
        let user = me(subject)?;
        if path.is_empty() || path.len() > 4096 {
            return Err(ServiceError::invalid("path", "invalid", "Choose a file."));
        }
        let keys = match mark {
            Viewed::File { hunks, .. } => hunks,
            Viewed::Hunk { key, .. } => std::slice::from_ref(key),
        };
        if keys.iter().any(|k| {
            k.len() != 64
                || !k
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        }) {
            return Err(ServiceError::invalid("hunk", "invalid", "Choose a hunk."));
        }
        let marking = matches!(
            mark,
            Viewed::File { viewed: true, .. } | Viewed::Hunk { viewed: true, .. }
        );
        if marking && !keys.is_empty() {
            let changed = self.mr_changed_paths(&m).await?;
            if !changed.iter().any(|p| p == path.as_bytes()) {
                return Err(ServiceError::invalid(
                    "path",
                    "unchanged",
                    "This merge request doesn't change that file.",
                ));
            }
            let held = db::viewed_hunk_count(&self.pool, m.id, user, path).await?;
            let max = i64::try_from(self.config.limits.diff_max_lines).unwrap_or(i64::MAX);
            if held.saturating_add(i64::try_from(keys.len()).unwrap_or(i64::MAX)) > max {
                return Err(ServiceError::invalid(
                    "hunk",
                    "limit",
                    format!("At most {max} hunks of one file can be marked viewed."),
                ));
            }
        }
        let mut tx = self.pool.begin().await.map_err(ServiceError::internal)?;
        match mark {
            Viewed::File { viewed, hunks } => {
                let blob = match (viewed, m.head_oid.as_deref()) {
                    (true, Some(head)) => Some(blob_at(self, &m, head, path).await?),
                    _ => None,
                };
                db::set_viewed(&mut *tx, m.id, user, path, blob.as_deref()).await?;
                db::set_viewed_hunks(&mut *tx, m.id, user, path, (hunks, viewed)).await?;
            }
            Viewed::Hunk { key, viewed } => {
                db::set_viewed_hunks(
                    &mut *tx,
                    m.id,
                    user,
                    path,
                    (std::slice::from_ref(key), viewed),
                )
                .await?;
                if !viewed {
                    db::set_viewed(&mut *tx, m.id, user, path, None).await?;
                }
            }
        }
        tx.commit().await.map_err(ServiceError::internal)?;
        Ok(())
    }
}
