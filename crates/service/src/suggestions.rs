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
    /// Whether the subject may apply suggestions to this merge request
    /// (the page offers it): `mr.apply_suggestion` or authorship, and the
    /// right to write its source branch.
    pub async fn may_apply_suggestions(
        &self,
        subject: &Subject,
        a: &crate::repos::RepoAccess,
        m: &MergeRequest,
    ) -> bool {
        let me = match subject {
            Subject::User { id, .. } => Some(*id),
            _ => None,
        };
        me.is_some()
            && (a.can(subject, Action::MrApplySuggestion) || m.author_id == me)
            && self.may_update_branch(subject, a, m).await
    }

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
        for (path, edits) in &mut files {
            edits.sort_by_key(|e| e.start);
            if edits.windows(2).any(|p| p[1].start <= p[0].end) {
                return Err(ServiceError::Conflict(format!(
                    "Two suggestions change the same lines of {path}; apply them one at a time."
                )));
            }
        }
        let tree = self.tree_with(&dir, &head, &files).await?;
        let all: Vec<&Edit> = files.values().flatten().collect();
        let new = match mode {
            ApplyMode::Commit => {
                self.suggestion_commit(&dir, &head, &tree, &all, *user)
                    .await?
            }
            ApplyMode::Fold => {
                self.fold_suggestions(&m, &dir, &head, &tree, &files, *user)
                    .await?
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

    /// The tree of `head` with `files`' edits (each file's sorted and
    /// disjoint) applied.
    async fn tree_with(
        &self,
        dir: &std::path::Path,
        head: &ObjectId,
        files: &BTreeMap<String, Vec<Edit>>,
    ) -> Result<ObjectId> {
        let mut tree = head.clone();
        for (path, edits) in files {
            let blob = self
                .runner
                .repo(dir)
                .args(["cat-file", "blob"])
                .arg(format!("{}:{path}", head.as_str()))
                .run()
                .await
                .map_err(internal)?
                .stdout;
            let content = splice(&blob, edits).ok_or_else(|| {
                ServiceError::Conflict(format!("A suggestion no longer fits {path}."))
            })?;
            tree = merge::replace_file(&self.runner, dir, &tree, path.as_bytes(), content)
                .await
                .map_err(internal)?
                .ok_or_else(|| ServiceError::Conflict(format!("{path} isn't a file anymore.")))?;
        }
        Ok(tree)
    }

    /// The commit of the stack that last changed `path`'s lines
    /// `start..=end` at `head`, if exactly one commit after `base` did.
    async fn introduced_by(
        &self,
        dir: &std::path::Path,
        (base, head): (Option<&str>, &ObjectId),
        e: &Edit,
        path: &str,
    ) -> Result<Option<ObjectId>> {
        let range = match base {
            Some(b) => format!("{b}..{}", head.as_str()),
            None => head.as_str().to_owned(),
        };
        let out = self
            .runner
            .repo(dir)
            .args(["blame", "--porcelain", "-L"])
            .arg(format!("{},{}", e.start, e.end))
            .args([range.as_str(), "--"])
            .arg(path)
            .run()
            .await
            .map_err(internal)?;
        let text = String::from_utf8_lossy(&out.stdout);
        let mut commits: Vec<&str> = Vec::new();
        let mut boundary = false;
        for line in text.lines() {
            let mut words = line.split(' ');
            match (words.next(), words.next()) {
                (Some(oid), Some(n))
                    if oid.len() == 40
                        && oid.bytes().all(|b| b.is_ascii_hexdigit())
                        && n.bytes().all(|b| b.is_ascii_digit()) =>
                {
                    if !commits.contains(&oid) {
                        commits.push(oid);
                    }
                }
                (Some("boundary"), None) => boundary = true,
                _ => {}
            }
        }
        Ok(match commits.as_slice() {
            [one] if !boundary => Some(ObjectId::parse(one).map_err(internal)?),
            _ => None,
        })
    }

    /// Folds each suggestion into the commit that introduced its lines and
    /// rebases the commits after it, keeping their authors, messages, and
    /// change IDs (D-38); the committer is the applier, and a folded
    /// commit gains a `Co-authored-by` trailer per suggester. The result
    /// must equal applying them on top (`all`), or nothing is written.
    async fn fold_suggestions(
        &self,
        m: &MergeRequest,
        dir: &std::path::Path,
        head: &ObjectId,
        all: &ObjectId,
        files: &BTreeMap<String, Vec<Edit>>,
        applier: slath_core::ids::UserId,
    ) -> Result<ObjectId> {
        let refuse = |why: &str| {
            ServiceError::Conflict(format!(
                "The suggestions can't be folded into their change: {why}; apply them as a new commit instead."
            ))
        };
        let (base, _) = self.revision_diff(m, m.revisions).await?;
        let base_oid = base
            .as_deref()
            .map(ObjectId::parse)
            .transpose()
            .map_err(internal)?;
        let max = self.config.limits.review_max_commits;
        let stack =
            slath_git::browse::log_range(&self.runner, dir, base_oid.as_ref(), head, max + 1)
                .await
                .map_err(internal)?;
        if stack.len() > max || stack.iter().any(|c| c.parents.len() > 1) {
            return Err(refuse("the branch has merge commits or too many commits"));
        }
        // The edits for each commit, as a tree from `head` with only them.
        let mut by_commit: BTreeMap<String, BTreeMap<String, Vec<Edit>>> = BTreeMap::new();
        for (path, edits) in files {
            for e in edits {
                let Some(c) = self
                    .introduced_by(dir, (base.as_deref(), head), e, path)
                    .await?
                else {
                    return Err(refuse(&format!(
                        "lines {}–{} of {path} don't come from one change of this merge request",
                        e.start, e.end
                    )));
                };
                by_commit
                    .entry(c.as_str().to_owned())
                    .or_default()
                    .entry(path.clone())
                    .or_default()
                    .push(e.clone());
            }
        }
        let mut fixes: BTreeMap<String, (ObjectId, Vec<slath_core::ids::UserId>)> = BTreeMap::new();
        for (c, files) in &by_commit {
            let tree = self.tree_with(dir, head, files).await?;
            let mut who = Vec::new();
            for e in files.values().flatten() {
                if let Some(u) = e.suggester
                    && !who.contains(&u)
                {
                    who.push(u);
                }
            }
            fixes.insert(c.clone(), (tree, who));
        }
        let head_tree = ObjectId::parse(
            self.runner
                .repo(dir)
                .args(["rev-parse", "--verify"])
                .arg(format!("{}^{{tree}}", head.as_str()))
                .run()
                .await
                .map_err(internal)?
                .stdout
                .trim_ascii(),
        )
        .map_err(internal)?;
        let ids = self.change_ids(m).await?;
        let committer = self.identity_of(applier).await?;
        let mut reader = slath_git::ObjectReader::open(&self.runner, dir)
            .await
            .map_err(internal)?;
        let mut onto: Option<ObjectId> = None;
        for c in &stack {
            let raw = reader.read_raw_commit(&c.oid).await.map_err(internal)?;
            let parent = c.parents.first();
            let unchanged_base = onto.is_none() || onto.as_ref() == parent;
            let fix = fixes.get(c.oid.as_str());
            if unchanged_base && fix.is_none() {
                // Before the first fold, commits stay as they are.
                onto = Some(c.oid.clone());
                continue;
            }
            let mut tree = if unchanged_base {
                ObjectId::parse(raw.tree().unwrap_or_default()).map_err(internal)?
            } else {
                let o = onto.clone().ok_or_else(|| internal("no base"))?;
                match merge::merge_trees(&self.runner, dir, parent, &o, &c.oid)
                    .await
                    .map_err(internal)?
                {
                    merge::TreeMerge::Clean(t) => t,
                    merge::TreeMerge::Conflicts(f) => {
                        return Err(refuse(&format!(
                            "a later commit conflicts in {}",
                            f.join(", ")
                        )));
                    }
                }
            };
            let mut message_extra = String::new();
            if let Some((fix_tree, who)) = fix {
                tree =
                    match merge::merge_trees(&self.runner, dir, Some(&head_tree), &tree, fix_tree)
                        .await
                        .map_err(internal)?
                    {
                        merge::TreeMerge::Clean(t) => t,
                        merge::TreeMerge::Conflicts(f) => {
                            return Err(refuse(&format!(
                                "the change conflicts in {}",
                                f.join(", ")
                            )));
                        }
                    };
                for u in who {
                    let id = self.identity_of(*u).await?;
                    let id = id.rsplitn(3, ' ').nth(2).unwrap_or(&id).to_owned();
                    message_extra.push_str(&format!("Co-authored-by: {id}\n"));
                }
            }
            let parents: Vec<ObjectId> = match &onto {
                Some(o) => vec![o.clone()],
                None => parent.cloned().into_iter().collect(),
            };
            let mut new = merge::rewrite(
                &raw,
                &tree,
                &parents,
                &committer,
                ids.get(c.oid.as_str()).map(String::as_str),
            );
            if !message_extra.is_empty() {
                new.message = with_trailers(&new.message, &message_extra);
            }
            onto = Some(
                merge::write_commit(&self.runner, dir, &new)
                    .await
                    .map_err(internal)?,
            );
        }
        let result = onto.ok_or_else(|| refuse("the merge request has no commits"))?;
        let result_tree = self
            .runner
            .repo(dir)
            .args(["rev-parse", "--verify"])
            .arg(format!("{}^{{tree}}", result.as_str()))
            .run()
            .await
            .map_err(internal)?
            .stdout;
        if result_tree.trim_ascii() != all.as_str().as_bytes() {
            return Err(refuse("the result would differ from applying them on top"));
        }
        Ok(result)
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

/// `message` with `trailers` (lines ending in `\n`) appended: to its
/// trailer block if it ends with one, else as a new paragraph.
fn with_trailers(message: &[u8], trailers: &str) -> Vec<u8> {
    let text = String::from_utf8_lossy(message);
    let body = text.trim_end_matches('\n');
    let last = body.rsplit('\n').next().unwrap_or("");
    let in_trailers = body.contains("\n\n")
        && last.split_once(": ").is_some_and(|(k, _)| {
            !k.is_empty() && k.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
        });
    let mut out = body.to_owned();
    out.push_str(if in_trailers { "\n" } else { "\n\n" });
    out.push_str(trailers);
    out.into_bytes()
}

#[cfg(test)]
mod tests {
    use super::{Edit, splice, with_trailers};

    #[test]
    fn trailers_join_the_trailer_block() {
        let t = "Co-authored-by: B <b@x>\n";
        assert_eq!(
            with_trailers(b"Fix\n", t),
            b"Fix\n\nCo-authored-by: B <b@x>\n"
        );
        assert_eq!(
            with_trailers(b"Fix\n\nBody.\n\nSigned-off-by: A <a@x>\n", t),
            b"Fix\n\nBody.\n\nSigned-off-by: A <a@x>\nCo-authored-by: B <b@x>\n"
        );
    }

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
