// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Server-side merges without a worktree: trees come from
//! `git merge-tree --write-tree`, commit objects are built byte for byte
//! and written with `git hash-object` (keeping `change-id` and unknown
//! headers, dropping signatures), and refs move only through
//! [`crate::refs::update_cas`].

use std::path::Path;

use crate::commit::{Header, RawCommit};
use crate::object::ObjectId;
use crate::{GitError, Runner};

/// The result of a three-way merge of trees.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TreeMerge {
    Clean(ObjectId),
    /// The paths with conflicts.
    Conflicts(Vec<String>),
}

/// Whether `a` is an ancestor of (or equal to) `b`.
pub async fn is_ancestor(
    runner: &Runner,
    git_dir: &Path,
    a: &ObjectId,
    b: &ObjectId,
) -> Result<bool, GitError> {
    let out = runner
        .repo(git_dir)
        .args([
            "merge-base",
            "--is-ancestor",
            "--end-of-options",
            a.as_str(),
            b.as_str(),
        ])
        .output()
        .await?;
    match out.status.code() {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        _ => Err(GitError::Failed {
            status: out.status,
            stderr: out.stderr,
        }),
    }
}

/// Merges `theirs` into `ours`, with `base` as the merge base (by default
/// Git's), and writes the resulting tree.
pub async fn merge_trees(
    runner: &Runner,
    git_dir: &Path,
    base: Option<&ObjectId>,
    ours: &ObjectId,
    theirs: &ObjectId,
) -> Result<TreeMerge, GitError> {
    let mut cmd = runner
        .repo(git_dir)
        .config("core.quotePath", "false")
        .args([
            "merge-tree",
            "--write-tree",
            "--name-only",
            "-z",
            "--no-messages",
        ]);
    if let Some(b) = base {
        cmd = cmd.arg(format!("--merge-base={}", b.as_str()));
    }
    let out = cmd
        .args(["--end-of-options", ours.as_str(), theirs.as_str()])
        .output()
        .await?;
    let mut parts = out.stdout.split(|b| *b == 0);
    let tree = parts.next().map(|t| t.trim_ascii()).unwrap_or_default();
    match out.status.code() {
        Some(0) => Ok(TreeMerge::Clean(
            ObjectId::parse(tree).map_err(|_| GitError::Protocol("merge-tree output"))?,
        )),
        Some(1) => {
            let mut files: Vec<String> = parts
                .take_while(|p| !p.is_empty())
                .map(|p| String::from_utf8_lossy(p).into_owned())
                .collect();
            files.dedup();
            Ok(TreeMerge::Conflicts(files))
        }
        _ => Err(GitError::Failed {
            status: out.status,
            stderr: out.stderr,
        }),
    }
}

/// Writes a commit object.
pub async fn write_commit(
    runner: &Runner,
    git_dir: &Path,
    c: &RawCommit,
) -> Result<ObjectId, GitError> {
    let out = runner
        .repo(git_dir)
        .args(["hash-object", "-t", "commit", "-w", "--stdin"])
        .stdin(c.to_bytes())
        .run()
        .await?;
    ObjectId::parse(out.stdout.trim_ascii()).map_err(|_| GitError::Protocol("hash-object output"))
}

fn header(name: &str, value: impl Into<Vec<u8>>) -> Header {
    Header {
        name: name.as_bytes().to_vec(),
        value: Some(value.into()),
    }
}

/// A rewritten copy of `c` (rebase): new tree, parents, and committer;
/// signatures dropped; `change-id` set to `change_id` if given (in place,
/// or after the committer). Every other header, including unknown ones,
/// keeps its bytes and order.
#[must_use]
pub fn rewrite(
    c: &RawCommit,
    tree: &ObjectId,
    parents: &[ObjectId],
    committer: &str,
    change_id: Option<&str>,
) -> RawCommit {
    let mut headers: Vec<Header> = Vec::with_capacity(c.headers.len() + 1);
    let mut wrote_parents = false;
    let mut wrote_change_id = false;
    for h in &c.headers {
        match h.name.as_slice() {
            b"tree" => headers.push(header("tree", tree.as_str())),
            b"parent" => {
                if !wrote_parents {
                    headers.extend(parents.iter().map(|p| header("parent", p.as_str())));
                    wrote_parents = true;
                }
            }
            b"committer" => {
                if !wrote_parents {
                    // A root commit gains its parents before the author.
                    wrote_parents = true;
                }
                headers.push(header("committer", committer));
            }
            b"gpgsig" | b"gpgsig-sha256" => {}
            b"change-id" => {
                if let Some(id) = change_id {
                    if !wrote_change_id {
                        headers.push(header("change-id", id));
                        wrote_change_id = true;
                    }
                } else {
                    headers.push(h.clone());
                }
            }
            b"author" if !wrote_parents => {
                headers.extend(parents.iter().map(|p| header("parent", p.as_str())));
                wrote_parents = true;
                headers.push(h.clone());
            }
            _ => headers.push(h.clone()),
        }
    }
    if let (Some(id), false) = (change_id, wrote_change_id) {
        let at = headers
            .iter()
            .position(|h| h.name == b"committer")
            .map_or(headers.len(), |i| i + 1);
        headers.insert(at, header("change-id", id));
    }
    RawCommit::new(headers, c.message.clone())
}

/// A new commit (merge commit or squash).
#[must_use]
pub fn new_commit(
    tree: &ObjectId,
    parents: &[ObjectId],
    author: &str,
    committer: &str,
    change_id: Option<&str>,
    message: &str,
) -> RawCommit {
    let mut headers = vec![header("tree", tree.as_str())];
    headers.extend(parents.iter().map(|p| header("parent", p.as_str())));
    headers.push(header("author", author));
    headers.push(header("committer", committer));
    if let Some(id) = change_id {
        headers.push(header("change-id", id));
    }
    let mut msg = message.trim_end().as_bytes().to_vec();
    msg.push(b'\n');
    RawCommit::new(headers, msg)
}

/// A tree entry as `git ls-tree -z` prints it: mode, type, object, name.
struct TreeEntry {
    mode: Vec<u8>,
    kind: Vec<u8>,
    oid: Vec<u8>,
    name: Vec<u8>,
}

async fn read_tree(
    runner: &Runner,
    git_dir: &Path,
    tree: &str,
) -> Result<Vec<TreeEntry>, GitError> {
    let out = runner
        .repo(git_dir)
        .args(["ls-tree", "-z", "--end-of-options", tree])
        .run()
        .await?;
    let mut entries = Vec::new();
    for rec in out.stdout.split(|&b| b == 0).filter(|r| !r.is_empty()) {
        let tab = rec
            .iter()
            .position(|&b| b == b'\t')
            .ok_or(GitError::Protocol("ls-tree output"))?;
        let mut meta = rec[..tab].splitn(3, |&b| b == b' ');
        let (Some(mode), Some(kind), Some(oid)) = (meta.next(), meta.next(), meta.next()) else {
            return Err(GitError::Protocol("ls-tree output"));
        };
        entries.push(TreeEntry {
            mode: mode.to_vec(),
            kind: kind.to_vec(),
            oid: oid.to_vec(),
            name: rec[tab + 1..].to_vec(),
        });
    }
    Ok(entries)
}

async fn write_tree(
    runner: &Runner,
    git_dir: &Path,
    entries: &[TreeEntry],
) -> Result<ObjectId, GitError> {
    let mut input = Vec::new();
    for e in entries {
        input.extend_from_slice(&e.mode);
        input.push(b' ');
        input.extend_from_slice(&e.kind);
        input.push(b' ');
        input.extend_from_slice(&e.oid);
        input.push(b'\t');
        input.extend_from_slice(&e.name);
        input.push(0);
    }
    let out = runner
        .repo(git_dir)
        .args(["mktree", "-z"])
        .stdin(input)
        .run()
        .await?;
    ObjectId::parse(out.stdout.trim_ascii()).map_err(|_| GitError::Protocol("mktree output"))
}

/// The tree of `commit` with the regular file at `path` replaced by
/// `content` (its mode kept), without a worktree or index. `None` if
/// there's no regular file at `path`.
pub async fn replace_file(
    runner: &Runner,
    git_dir: &Path,
    commit: &ObjectId,
    path: &[u8],
    content: Vec<u8>,
) -> Result<Option<ObjectId>, GitError> {
    let out = runner
        .repo(git_dir)
        .args(["hash-object", "-t", "blob", "-w", "--stdin"])
        .stdin(content)
        .run()
        .await?;
    let blob = out.stdout.trim_ascii().to_vec();
    let parts: Vec<&[u8]> = path.split(|&b| b == b'/').collect();
    if parts
        .iter()
        .any(|p| p.is_empty() || *p == b"." || *p == b"..")
    {
        return Ok(None);
    }
    // The trees along the path, root first.
    let mut trees = vec![format!("{}^{{tree}}", commit.as_str())];
    let mut levels = Vec::new();
    for (i, part) in parts.iter().enumerate() {
        let entries = read_tree(runner, git_dir, &trees[i]).await?;
        let Some(pos) = entries.iter().position(|e| e.name == *part) else {
            return Ok(None);
        };
        let last = i + 1 == parts.len();
        let e = &entries[pos];
        if last {
            if e.kind != b"blob" || !(e.mode == b"100644" || e.mode == b"100755") {
                return Ok(None);
            }
        } else if e.kind != b"tree" {
            return Ok(None);
        } else {
            trees.push(String::from_utf8_lossy(&e.oid).into_owned());
        }
        levels.push((entries, pos));
    }
    // Rewrite bottom-up: each level points at the new object below it.
    let mut new = blob;
    for (mut entries, pos) in levels.into_iter().rev() {
        entries[pos].oid = new;
        new = write_tree(runner, git_dir, &entries)
            .await?
            .as_str()
            .as_bytes()
            .to_vec();
    }
    ObjectId::parse(&new)
        .map(Some)
        .map_err(|_| GitError::Protocol("mktree output"))
}

#[cfg(test)]
mod tests {
    use super::*;

    const T: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";
    const P: &str = "1111111111111111111111111111111111111111";
    const Q: &str = "2222222222222222222222222222222222222222";

    fn oid(s: &str) -> ObjectId {
        ObjectId::parse(s).unwrap()
    }

    #[test]
    fn rewrite_keeps_headers_and_drops_signatures() {
        let raw = format!(
            "tree {T}\nparent {P}\nauthor A <a@x> 1 +0000\ncommitter A <a@x> 1 +0000\n\
             gpgsig -----BEGIN PGP SIGNATURE-----\n sig\n -----END PGP SIGNATURE-----\n\
             change-id {}\nx-unknown keep me\n\nmsg\n",
            "k".repeat(32)
        );
        let c = RawCommit::parse(raw.as_bytes()).unwrap();
        let r = rewrite(&c, &oid(T), &[oid(Q)], "M <m@x> 2 +0000", None);
        let text = String::from_utf8(r.to_bytes()).unwrap();
        assert_eq!(
            text,
            format!(
                "tree {T}\nparent {Q}\nauthor A <a@x> 1 +0000\ncommitter M <m@x> 2 +0000\n\
                 change-id {}\nx-unknown keep me\n\nmsg\n",
                "k".repeat(32)
            )
        );
    }

    #[test]
    fn rewrite_adds_the_change_id_after_the_committer() {
        let raw = format!(
            "tree {T}\nparent {P}\nauthor A <a@x> 1 +0000\ncommitter A <a@x> 1 +0000\n\nmsg\n"
        );
        let c = RawCommit::parse(raw.as_bytes()).unwrap();
        let id = "z".repeat(32);
        let r = rewrite(&c, &oid(T), &[oid(Q)], "M <m@x> 2 +0000", Some(&id));
        assert_eq!(r.change_id(), Some(id.as_str()));
        let text = String::from_utf8(r.to_bytes()).unwrap();
        assert!(
            text.contains(&format!("committer M <m@x> 2 +0000\nchange-id {id}\n\nmsg")),
            "{text}"
        );
        // A root commit gains a parent before the author.
        let root = RawCommit::parse(format!(
            "tree {T}\nauthor A <a@x> 1 +0000\ncommitter A <a@x> 1 +0000\n\nm\n"
        ))
        .unwrap();
        let r = rewrite(&root, &oid(T), &[oid(Q)], "M <m@x> 2 +0000", None);
        assert!(
            String::from_utf8(r.to_bytes())
                .unwrap()
                .starts_with(&format!("tree {T}\nparent {Q}\nauthor"))
        );
    }

    #[test]
    fn new_commits() {
        let c = new_commit(
            &oid(T),
            &[oid(P), oid(Q)],
            "A <a@x> 1 +0000",
            "M <m@x> 2 +0000",
            None,
            "Merge !1: x\n\n",
        );
        assert_eq!(c.parents().count(), 2);
        assert!(c.to_bytes().ends_with(b"\n\nMerge !1: x\n"));
    }
}
