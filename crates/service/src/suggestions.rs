// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Applying suggestions (spec: comments, batched reviews, suggestions;
//! D-44): a ```` ```suggestion ```` block in a line comment replaces the
//! commented lines. One or several suggestions are applied together on the
//! server, the same way for everyone, Git or jj:
//!
//! - as a new commit on top of the stack, whose author is the suggester
//!   (the applier when several people suggested), whose committer is the
//!   applier, with a `Co-authored-by` trailer for each suggester;
//! - or folded into the change that introduced the lines (next commit).
//!
//! A suggestion applies to the current head only where its lines are
//! unchanged since it was made (the thread isn't outdated). The result
//! goes through push protection and the source branch's protection, and
//! the branch moves by compare-and-swap like any server-side write.

use std::collections::BTreeMap;

use slath_core::authz::{Action, Subject};
use slath_db::merge_requests::MergeRequest;
use slath_git::ObjectId;
use slath_git::merge;

use crate::Services;
use crate::error::{Result, ServiceError};
use crate::mr_threads::Position;

fn internal(e: impl std::fmt::Display) -> ServiceError {
    ServiceError::internal(e.to_string())
}

/// How applied suggestions land on the branch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplyMode {
    /// A new commit on top.
    Commit,
    /// Folded into the change that introduced the lines.
    Fold,
}

impl ApplyMode {
    /// `commit` or `fold`.
    pub fn parse(s: &str) -> Result<Self> {
        match s {
            "" | "commit" => Ok(Self::Commit),
            "fold" => Ok(Self::Fold),
            _ => Err(ServiceError::invalid(
                "mode",
                "invalid",
                "Choose commit or fold.",
            )),
        }
    }
}

/// One suggestion, placed in the current head.
#[derive(Debug, Clone)]
struct Edit {
    comment: uuid::Uuid,
    thread: uuid::Uuid,
    suggester: Option<slath_core::ids::UserId>,
    /// 1-based, inclusive.
    start: usize,
    end: usize,
    text: String,
}

/// `lines` with `start..=end` (1-based) replaced by `text`. The file keeps
/// a missing final newline if the replaced lines ended it.
fn splice(file: &[u8], edits: &[Edit]) -> Option<Vec<u8>> {
    let lines: Vec<&[u8]> = file.split_inclusive(|&b| b == b'\n').collect();
    let mut out = Vec::with_capacity(file.len());
    let mut next = 1;
    for e in edits {
        if e.start < next || e.end > lines.len() || e.end < e.start {
            return None;
        }
        for l in &lines[next - 1..e.start - 1] {
            out.extend_from_slice(l);
        }
        let mut text = e.text.clone().into_bytes();
        let at_eof = e.end == lines.len() && !lines[e.end - 1].ends_with(b"\n");
        if at_eof && text.last() == Some(&b'\n') {
            text.pop();
        }
        out.extend_from_slice(&text);
        next = e.end + 1;
    }
    for l in lines.iter().skip(next - 1) {
        out.extend_from_slice(l);
    }
    Some(out)
}

impl Services {
    /// Applies the suggestions of `comments` (each a published line comment
    /// with a suggestion block) to the source branch.
    pub async fn apply_suggestions(
        &self,
        subject: &Subject,
        (ns, repo, number): (&str, &str, i32),
        comments: &[uuid::Uuid],
        mode: ApplyMode,
        expected_head_oid: Option<&str>,
    ) -> Result<MergeRequest> {
        let (a, m) = self.mr(subject, ns, repo, number).await?;
        let Subject::User { id: user, .. } = subject else {
            return Err(ServiceError::Unauthenticated);
        };
        if !a.can(subject, Action::MrApplySuggestion) && m.author_id != Some(*user) {
            return Err(ServiceError::Forbidden);
        }
        if m.state != "open" {
            return Err(ServiceError::Conflict(
                "The merge request isn't open.".into(),
            ));
        }
        if comments.is_empty() {
            return Err(ServiceError::invalid(
                "comments",
                "required",
                "Choose at least one suggestion.",
            ));
        }
        let head = m.head_oid.clone().ok_or_else(|| {
            ServiceError::Conflict("The merge request has no revision yet.".into())
        })?;
        if expected_head_oid.is_some_and(|e| e != head) {
            return Err(ServiceError::Conflict(format!(
                "The merge request changed since you looked: it's at revision {} now.",
                m.revisions
            )));
        }
        let head = ObjectId::parse(&head).map_err(internal)?;
        let w = self.source_write(subject, &a, &m, &head).await?;

        // Every suggestion, placed in the current head, by file.
        let threads = self.mr_threads(subject, &a, &m).await?;
        let mut files: BTreeMap<String, Vec<Edit>> = BTreeMap::new();
        for id in comments {
            let Some((t, c)) = threads.iter().find_map(|t| {
                t.comments
                    .iter()
                    .find(|c| c.id == *id && !c.pending && c.deleted_at.is_none())
                    .map(|c| (t, c))
            }) else {
                return Err(ServiceError::NotFound);
            };
            let Some(text) = slath_markup::suggestion::suggestion(&c.body) else {
                return Err(ServiceError::invalid(
                    "comments",
                    "no_suggestion",
                    "A chosen comment has no suggestion.",
                ));
            };
            let Position::Current {
                path,
                side,
                line_start,
                line_end,
                ..
            } = &t.position
            else {
                return Err(ServiceError::Conflict(
                    "A suggestion is outdated: its lines changed since it was made.".into(),
                ));
            };
            if side != "new" {
                return Err(ServiceError::invalid(
                    "comments",
                    "old_side",
                    "A suggestion on removed lines can't be applied.",
                ));
            }
            let (Ok(start), Ok(end)) = (usize::try_from(*line_start), usize::try_from(*line_end))
            else {
                return Err(internal("bad line numbers"));
            };
            files.entry(path.clone()).or_default().push(Edit {
                comment: c.id,
                thread: t.thread.id,
                suggester: c.author_id,
                start,
                end,
                text,
            });
        }

        // Push protection looks at what the suggestions add.
        if self.push_protection_mode(a.repo.id).await? == slath_core::config::ScanMode::Block {
            let added: String = files.values().flatten().map(|e| e.text.as_str()).collect();
            let blocking: Vec<String> = self
                .scan_rules
                .scan(added.as_bytes())
                .into_iter()
                .filter(|f| f.tier == slath_core::secret_scan::Tier::Block)
                .map(|f| f.description)
                .collect();
            if !blocking.is_empty() {
                return Err(ServiceError::invalid(
                    "comments",
                    "secret",
                    format!(
                        "A suggestion contains what looks like a secret ({}); it can't be applied.",
                        blocking.join(", ")
                    ),
                ));
            }
        }

        let dir = self.repo_path(a.repo.id);
        let mut tree = head.clone();
        for (path, edits) in &mut files {
            edits.sort_by_key(|e| e.start);
            if edits.windows(2).any(|p| p[1].start <= p[0].end) {
                return Err(ServiceError::Conflict(format!(
                    "Two suggestions change the same lines of {path}; apply them one at a time."
                )));
            }
            let blob = self
                .runner
                .repo(&dir)
                .args(["cat-file", "blob"])
                .arg(format!("{}:{path}", head.as_str()))
                .run()
                .await
                .map_err(internal)?
                .stdout;
            let content = splice(&blob, edits).ok_or_else(|| {
                ServiceError::Conflict(format!("A suggestion no longer fits {path}."))
            })?;
            tree = merge::replace_file(&self.runner, &dir, &tree, path.as_bytes(), content)
                .await
                .map_err(internal)?
                .ok_or_else(|| ServiceError::Conflict(format!("{path} isn't a file anymore.")))?;
        }
        let all: Vec<&Edit> = files.values().flatten().collect();
        let new = match mode {
            ApplyMode::Commit => {
                self.suggestion_commit(&dir, &head, &tree, &all, *user)
                    .await?
            }
            ApplyMode::Fold => {
                return Err(ServiceError::invalid(
                    "mode",
                    "unavailable",
                    "Folding suggestions into their change isn't available yet.",
                ));
            }
        };
        if let Some(why) = self
            .source_protection_refusal(&w, mode == ApplyMode::Fold)
            .await?
        {
            return Err(ServiceError::Conflict(format!(
                "The suggestions can't be applied: {why}."
            )));
        }
        self.move_source(&w, &a, &m, &new, *user).await?;

        // Applied suggestions resolve their threads.
        let mut tx = self.pool.begin().await.map_err(internal)?;
        for e in &all {
            if threads
                .iter()
                .any(|t| t.thread.id == e.thread && t.thread.resolved_at.is_none())
            {
                slath_db::mr_threads::set_resolved(&mut *tx, e.thread, Some(*user)).await?;
                slath_db::issues::insert_event(
                    &mut *tx,
                    a.repo.id,
                    "merge_request",
                    *m.id.as_uuid(),
                    Some(*user),
                    "thread_resolved",
                    &serde_json::json!({"thread": e.thread, "suggestion": e.comment}),
                )
                .await?;
            }
        }
        tx.commit().await.map_err(internal)?;
        self.audit(
            subject,
            if w.maintainer_edit {
                "push.maintainer_edit"
            } else {
                "mr.apply_suggestion"
            },
            crate::audit::Target::new("merge_request", Some(*m.id.as_uuid()))
                .in_repo(*a.repo.id.as_uuid()),
            serde_json::json!({
                "number": m.number,
                "mode": if mode == ApplyMode::Fold { "fold" } else { "commit" },
                "comments": all.iter().map(|e| e.comment.to_string()).collect::<Vec<_>>(),
                "oid": new.as_str(),
            }),
        )
        .await;
        slath_db::merge_requests::get_by_id(&self.pool, m.id)
            .await?
            .ok_or(ServiceError::NotFound)
    }

    /// The commit on top of `head` with the applied suggestions: authored
    /// by the suggester (the applier when several people suggested),
    /// committed by the applier, with a `Co-authored-by` trailer for every
    /// suggester.
    async fn suggestion_commit(
        &self,
        dir: &std::path::Path,
        head: &ObjectId,
        tree: &ObjectId,
        edits: &[&Edit],
        applier: slath_core::ids::UserId,
    ) -> Result<ObjectId> {
        let mut suggesters: Vec<slath_core::ids::UserId> = Vec::new();
        for e in edits {
            if let Some(u) = e.suggester
                && !suggesters.contains(&u)
            {
                suggesters.push(u);
            }
        }
        let committer = self.identity_of(applier).await?;
        let author = match suggesters.as_slice() {
            [one] => self.identity_of(*one).await?,
            _ => committer.clone(),
        };
        let mut message = if edits.len() == 1 {
            "Apply suggestion from review".to_owned()
        } else {
            format!("Apply {} suggestions from review", edits.len())
        };
        if !suggesters.is_empty() {
            message.push('\n');
            for u in &suggesters {
                // `Name <email> <time> <tz>` without the time.
                let who = self.identity_of(*u).await?;
                let who = who.rsplitn(3, ' ').nth(2).unwrap_or(&who).to_owned();
                message.push_str(&format!("\nCo-authored-by: {who}"));
            }
        }
        let c = merge::new_commit(
            tree,
            std::slice::from_ref(head),
            &author,
            &committer,
            None,
            &message,
        );
        merge::write_commit(&self.runner, dir, &c)
            .await
            .map_err(internal)
    }
}

#[cfg(test)]
mod tests {
    use super::{Edit, splice};

    fn edit(start: usize, end: usize, text: &str) -> Edit {
        Edit {
            comment: uuid::Uuid::nil(),
            thread: uuid::Uuid::nil(),
            suggester: None,
            start,
            end,
            text: text.to_owned(),
        }
    }

    #[test]
    fn splice_replaces_lines() {
        let f = b"a\nb\nc\nd";
        assert_eq!(splice(f, &[edit(2, 3, "X\n")]).unwrap(), b"a\nX\nd");
        assert_eq!(
            splice(f, &[edit(1, 1, ""), edit(4, 4, "D\n")]).unwrap(),
            b"b\nc\nD"
        );
        assert_eq!(
            splice(b"a\nb\n", &[edit(2, 2, "B\nC\n")]).unwrap(),
            b"a\nB\nC\n"
        );
        assert!(splice(f, &[edit(4, 5, "x\n")]).is_none());
    }
}
