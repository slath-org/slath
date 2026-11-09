// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Comparing and syncing a fork's branch with its parent's.

use std::path::Path;

use crate::args::RefName;
use crate::object::ObjectId;
use crate::{GitError, Runner};

/// Commits on `ours` but not on `theirs`, and the other way round. `ours`
/// is in `fork`, `theirs` in `parent`; neither repository is changed.
pub async fn ahead_behind(
    runner: &Runner,
    fork: &Path,
    parent: &Path,
    ours: &ObjectId,
    theirs: &ObjectId,
) -> Result<(u32, u32), GitError> {
    if ours == theirs {
        return Ok((0, 0));
    }
    let out = runner
        .repo(fork)
        .read_objects_of(parent)?
        .args(["rev-list", "--left-right", "--count", "--end-of-options"])
        .arg(format!("{}...{}", ours.as_str(), theirs.as_str()))
        .run()
        .await?;
    let text = String::from_utf8_lossy(&out.stdout);
    let mut it = text.split_whitespace().map(str::parse::<u32>);
    match (it.next(), it.next()) {
        (Some(Ok(a)), Some(Ok(b))) => Ok((a, b)),
        _ => Err(GitError::Protocol("rev-list --count output")),
    }
}

/// Fetches `branch` of `parent` into the fork under a temporary internal
/// ref (removed again), so the fork has its objects. Returns the fetched
/// tip, or `None` if the parent has no such branch.
pub async fn fetch_parent_branch(
    runner: &Runner,
    fork: &Path,
    parent: &Path,
    branch: &RefName,
) -> Result<Option<ObjectId>, GitError> {
    if crate::refs::read(runner, parent, branch).await?.is_none() {
        return Ok(None);
    }
    let tmp = crate::refs::sync_temp();
    let mut spec = b"+".to_vec();
    spec.extend_from_slice(branch.as_bytes());
    spec.push(b':');
    spec.extend_from_slice(tmp.as_bytes());
    let fetched = runner
        .repo(fork)
        .config("protocol.file.allow", "always")
        .args([
            "fetch",
            "--quiet",
            "--no-tags",
            "--no-write-fetch-head",
            "--",
        ])
        .arg(parent)
        .arg_bytes(&spec)
        .timeout(std::time::Duration::from_secs(3600))
        .run()
        .await;
    let tip = match fetched {
        Ok(_) => crate::refs::read(runner, fork, &tmp).await,
        Err(e) => Err(e),
    };
    let _ = runner
        .repo(fork)
        .args(["update-ref", "-d", "--end-of-options"])
        .arg_bytes(tmp.as_bytes())
        .run()
        .await;
    tip
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

/// Fetches exactly `tip` (and its history) of `source` into `target` as
/// `dest`, a reserved ref. Nothing else of `source` comes along: no tags,
/// no other branches. Protocol v2 serves any object by ID, so the commit
/// needn't be a ref tip when the fetch runs.
pub async fn fetch_commit(
    runner: &Runner,
    target: &Path,
    source: &Path,
    tip: &ObjectId,
    dest: &RefName,
) -> Result<(), GitError> {
    let mut spec = format!("+{}:", tip.as_str()).into_bytes();
    spec.extend_from_slice(dest.as_bytes());
    runner
        .repo(target)
        .config("protocol.file.allow", "always")
        .config("protocol.version", "2")
        .args([
            "fetch",
            "--quiet",
            "--no-tags",
            "--no-write-fetch-head",
            "--",
        ])
        .arg(source)
        .arg_bytes(&spec)
        .timeout(std::time::Duration::from_secs(3600))
        .run()
        .await?;
    Ok(())
}
