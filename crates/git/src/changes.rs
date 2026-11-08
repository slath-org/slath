// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! What change identity needs from the commits of a merge request
//! revision: jj's header, the `Change-Id` trailer, the patch and its
//! `git patch-id --stable`, and whether jj recorded a conflict.

use std::collections::HashMap;
use std::path::Path;

use crate::object::ObjectId;
use crate::{GitError, ObjectReader, Runner};

/// One commit of a revision's stack.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StackCommit {
    pub oid: ObjectId,
    pub parent: Option<ObjectId>,
    /// The `change-id` header, if there's exactly one and it's valid.
    pub change_id: Option<String>,
    /// The value of the last `Change-Id` trailer.
    pub trailer: Option<String>,
    /// `git patch-id --stable`; `None` for an empty commit or when the
    /// patches were too large to read.
    pub patch_id: Option<String>,
    /// The commit's patch (empty when too large).
    pub patch: Vec<u8>,
    /// jj recorded a conflict: a `jj:trees` header, or `.jjconflict-*`
    /// entries in the root tree.
    pub conflicted: bool,
}

/// The value of the last `Change-Id` trailer of a commit message: in the
/// message's last paragraph, which must consist of trailer lines
/// (`Token: value`, with indented continuation lines), as Git reads them.
#[must_use]
pub fn change_id_trailer(message: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(message);
    let body = text.trim_end();
    let last = body.rsplit("\n\n").next()?;
    // The subject alone isn't a trailer block.
    if last.len() == body.len() {
        return None;
    }
    let mut found = None;
    for line in last.lines() {
        if line.starts_with([' ', '\t']) {
            continue;
        }
        let (key, value) = line.split_once(':')?;
        if key.is_empty() || !key.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') {
            return None;
        }
        if key.eq_ignore_ascii_case("change-id") {
            found = Some(value.trim().to_owned());
        }
    }
    found
}

/// Reads the stack commits `oids` (bottom first). Patches larger than
/// `max_patch_bytes` in total are left out, as are all patches with 0;
/// matching then relies on aliases.
pub async fn stack(
    runner: &Runner,
    git_dir: &Path,
    oids: &[ObjectId],
    max_patch_bytes: usize,
) -> Result<Vec<StackCommit>, GitError> {
    if oids.is_empty() {
        return Ok(Vec::new());
    }
    let mut reader = ObjectReader::open(runner, git_dir).await?;
    let mut out = Vec::with_capacity(oids.len());
    for oid in oids {
        let c = reader.read_raw_commit(oid).await?;
        let change_id = c.change_id().map(str::to_owned);
        let mut conflicted = c.has_jj_trees_header();
        if !conflicted && let Some(tree) = c.tree().and_then(|t| ObjectId::parse(t).ok()) {
            conflicted = reader.read_tree(&tree).await?.iter().any(|e| {
                e.name.starts_with(b".jjconflict-base-") || e.name.starts_with(b".jjconflict-side-")
            });
        }
        out.push(StackCommit {
            oid: oid.clone(),
            parent: c.parents().next().and_then(|p| ObjectId::parse(p).ok()),
            change_id,
            trailer: change_id_trailer(&c.message),
            patch_id: None,
            patch: Vec::new(),
            conflicted,
        });
    }
    if max_patch_bytes == 0 {
        return Ok(out);
    }
    let mut cmd = runner
        .repo(git_dir)
        .config("core.quotePath", "false")
        .args([
            "log",
            "--no-walk=unsorted",
            "--no-color",
            "--no-ext-diff",
            "--no-textconv",
            "--full-index",
            "--patch",
            "--format=commit %H",
            "--end-of-options",
        ]);
    for oid in oids {
        cmd = cmd.arg(oid.as_str());
    }
    let log = match cmd.arg("--").max_output(max_patch_bytes).run().await {
        Ok(o) => o.stdout,
        Err(GitError::Limit(crate::LimitKind::Output)) => return Ok(out),
        Err(e) => return Err(e),
    };
    let ids = runner
        .repo(git_dir)
        .args(["patch-id", "--stable"])
        .stdin(log.clone())
        .run()
        .await?
        .stdout;
    let mut patch_ids: HashMap<String, String> = HashMap::new();
    for line in String::from_utf8_lossy(&ids).lines() {
        if let Some((pid, commit)) = line.split_once(' ') {
            patch_ids.insert(commit.to_owned(), pid.to_owned());
        }
    }
    let mut patches: HashMap<String, Vec<u8>> = HashMap::new();
    let mut current: Option<(String, Vec<u8>)> = None;
    for line in log.split_inclusive(|b| *b == b'\n') {
        if let Some(rest) = line.strip_prefix(b"commit ") {
            if let Some((k, v)) = current.take() {
                patches.insert(k, v);
            }
            let oid = String::from_utf8_lossy(rest).trim().to_owned();
            current = Some((oid, Vec::new()));
        } else if let Some((_, v)) = current.as_mut() {
            v.extend_from_slice(line);
        }
    }
    if let Some((k, v)) = current {
        patches.insert(k, v);
    }
    for c in &mut out {
        c.patch_id = patch_ids.remove(c.oid.as_str());
        c.patch = patches.remove(c.oid.as_str()).unwrap_or_default();
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::change_id_trailer;

    #[test]
    fn trailers() {
        let i = "I0123456789abcdef0123456789abcdef01234567";
        assert_eq!(
            change_id_trailer(format!("Subject\n\nBody.\n\nChange-Id: {i}\n").as_bytes()),
            Some(i.to_owned())
        );
        // The last one wins; other trailers and continuations are fine.
        assert_eq!(
            change_id_trailer(
                format!("S\n\nChange-Id: Iold\nSigned-off-by: A <a@b>\n  more\nChange-Id: {i}\n")
                    .as_bytes()
            ),
            Some(i.to_owned())
        );
        // Not in the last paragraph, or the paragraph isn't all trailers.
        assert_eq!(
            change_id_trailer(format!("S\n\nChange-Id: {i}\n\nText.\n").as_bytes()),
            None
        );
        assert_eq!(
            change_id_trailer(format!("S\n\nSome text\nChange-Id: {i}\n").as_bytes()),
            None
        );
        // The subject line alone is no trailer block.
        assert_eq!(
            change_id_trailer(format!("Change-Id: {i}\n").as_bytes()),
            None
        );
    }
}
