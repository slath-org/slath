// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Ref writes by the server, and the names of the refs it reserves.
//!
//! Every ref the server writes goes through [`update_cas`]: the update
//! names the value it expects to replace, so a concurrent push or another
//! server task can never be overwritten silently.
//!
//! Reserved refs (clients can't push them):
//! - `refs/slath/mr/<mr-id>/r<seq>` pins each merge request revision so
//!   garbage collection keeps it; hidden from fetches.
//! - `refs/slath/mr/<mr-id>/incoming` holds the latest fetched head of a
//!   merge request from a fork; hidden from fetches.
//! - `refs/merge-requests/<n>/head` points at a merge request's current
//!   head; advertised, so clients can fetch it.

use std::path::Path;

use slath_core::ids::MergeRequestId;

use crate::args::RefName;
use crate::error::GitError;
use crate::object::ObjectId;
use crate::runner::Runner;

/// Why a compare-and-swap update didn't happen.
#[derive(Debug, thiserror::Error)]
pub enum RefUpdateError {
    /// The ref doesn't have the expected value; `current` is what it has.
    #[error("the ref changed concurrently")]
    Conflict { current: Option<ObjectId> },
    #[error(transparent)]
    Git(#[from] GitError),
}

/// The pin of a merge request revision.
#[must_use]
pub fn mr_revision_pin(mr: MergeRequestId, seq: u32) -> RefName {
    reserved(format!("refs/slath/mr/{}/r{seq}", mr.as_uuid()))
}

/// The latest fetched head of a merge request from a fork.
#[must_use]
pub fn mr_incoming(mr: MergeRequestId) -> RefName {
    reserved(format!("refs/slath/mr/{}/incoming", mr.as_uuid()))
}

/// The advertised head of merge request `number`.
#[must_use]
pub fn mr_head(number: u32) -> RefName {
    reserved(format!("refs/merge-requests/{number}/head"))
}

fn reserved(name: String) -> RefName {
    // Built from a UUID or numbers only, so always a valid ref name.
    RefName::parse(name).unwrap_or_else(|_| unreachable!("reserved ref names are valid"))
}

/// The value of a ref, or `None` if it doesn't exist. Symbolic refs aren't
/// followed: the server never creates any.
pub async fn read(
    runner: &Runner,
    git_dir: &Path,
    name: &RefName,
) -> Result<Option<ObjectId>, GitError> {
    let out = runner
        .repo(git_dir)
        .args(["for-each-ref", "--format=%(objectname) %(refname)"])
        .arg("--end-of-options")
        .arg_bytes(name.as_bytes())
        .output()
        .await?;
    if !out.status.success() {
        return Err(GitError::Failed {
            status: out.status,
            stderr: out.stderr,
        });
    }
    // `for-each-ref` matches prefixes at `/` boundaries; keep the exact name.
    for line in out.stdout.split(|&b| b == b'\n') {
        if let Some(sp) = line.iter().position(|&b| b == b' ')
            && &line[sp + 1..] == name.as_bytes()
        {
            return ObjectId::parse(&line[..sp])
                .map(Some)
                .map_err(GitError::Protocol);
        }
    }
    Ok(None)
}

/// Sets `name` to `new` if it currently is `expected` (`None`: it must not
/// exist). `new = None` deletes the ref. On a mismatch, nothing changes and
/// the current value is returned in [`RefUpdateError::Conflict`].
pub async fn update_cas(
    runner: &Runner,
    git_dir: &Path,
    name: &RefName,
    new: Option<&ObjectId>,
    expected: Option<&ObjectId>,
) -> Result<(), RefUpdateError> {
    const ZERO: &str = "0000000000000000000000000000000000000000";
    let mut line = Vec::new();
    match new {
        Some(n) => {
            line.extend_from_slice(b"update ");
            line.extend_from_slice(name.as_bytes());
            line.push(0);
            line.extend_from_slice(n.as_str().as_bytes());
        }
        None => {
            line.extend_from_slice(b"delete ");
            line.extend_from_slice(name.as_bytes());
        }
    }
    line.push(0);
    line.extend_from_slice(expected.map_or(ZERO, ObjectId::as_str).as_bytes());
    line.push(0);
    let out = runner
        .repo(git_dir)
        .args(["update-ref", "--no-deref", "-z", "--stdin"])
        .stdin(line)
        .output()
        .await?;
    if out.status.success() {
        return Ok(());
    }
    // Tell a lost race from other failures by looking at the ref again.
    let current = read(runner, git_dir, name).await?;
    if current.as_ref() != expected {
        return Err(RefUpdateError::Conflict { current });
    }
    Err(RefUpdateError::Git(GitError::Failed {
        status: out.status,
        stderr: out.stderr,
    }))
}

/// Points `name` at `new` whatever it was before, for refs only the server
/// writes. Each step is still a compare-and-swap, retried a few times when
/// another task moved the ref in between.
pub async fn set(
    runner: &Runner,
    git_dir: &Path,
    name: &RefName,
    new: &ObjectId,
) -> Result<(), RefUpdateError> {
    let mut current = read(runner, git_dir, name).await?;
    for _ in 0..5 {
        if current.as_ref() == Some(new) {
            return Ok(());
        }
        match update_cas(runner, git_dir, name, Some(new), current.as_ref()).await {
            Err(RefUpdateError::Conflict { current: c }) => current = c,
            other => return other,
        }
    }
    Err(RefUpdateError::Conflict { current })
}

/// True if `oid` names a commit in the repository.
pub async fn commit_exists(
    runner: &Runner,
    git_dir: &Path,
    oid: &ObjectId,
) -> Result<bool, GitError> {
    let out = runner
        .repo(git_dir)
        .args(["cat-file", "-e"])
        .arg(format!("{}^{{commit}}", oid.as_str()))
        .output()
        .await?;
    Ok(out.status.success())
}

/// Pins revision `seq` of a merge request at `head`. Pinning the same
/// commit again is a no-op; a pin is never moved to another commit.
pub async fn pin_revision(
    runner: &Runner,
    git_dir: &Path,
    mr: MergeRequestId,
    seq: u32,
    head: &ObjectId,
) -> Result<(), RefUpdateError> {
    let name = mr_revision_pin(mr, seq);
    match update_cas(runner, git_dir, &name, Some(head), None).await {
        Err(RefUpdateError::Conflict { current: Some(c) }) if &c == head => Ok(()),
        other => other,
    }
}

/// Removes every reserved ref of a merge request (its pins and incoming
/// head) and its advertised head, when the merge request itself is deleted.
pub async fn remove_mr_refs(
    runner: &Runner,
    git_dir: &Path,
    mr: MergeRequestId,
    number: u32,
) -> Result<(), RefUpdateError> {
    let prefix = format!("refs/slath/mr/{}/", mr.as_uuid());
    let out = runner
        .repo(git_dir)
        .args(["for-each-ref", "--format=%(objectname) %(refname)"])
        .arg("--end-of-options")
        .arg(&prefix)
        .run()
        .await?;
    let mut refs: Vec<(RefName, ObjectId)> = Vec::new();
    for line in out.stdout.split(|&b| b == b'\n').filter(|l| !l.is_empty()) {
        if let Some(sp) = line.iter().position(|&b| b == b' ')
            && let (Ok(oid), Ok(name)) = (
                ObjectId::parse(&line[..sp]),
                RefName::parse(&line[sp + 1..]),
            )
        {
            refs.push((name, oid));
        }
    }
    let head = mr_head(number);
    if let Some(oid) = read(runner, git_dir, &head).await? {
        refs.push((head, oid));
    }
    for (name, oid) in refs {
        match update_cas(runner, git_dir, &name, None, Some(&oid)).await {
            Ok(()) | Err(RefUpdateError::Conflict { current: None }) => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reserved_names() {
        let id = MergeRequestId::new();
        let pin = mr_revision_pin(id, 3);
        assert!(pin.is_reserved());
        assert_eq!(
            pin.as_bytes(),
            format!("refs/slath/mr/{}/r3", id.as_uuid()).as_bytes()
        );
        assert!(mr_incoming(id).is_reserved());
        assert_eq!(mr_head(12).as_bytes(), b"refs/merge-requests/12/head");
        assert!(mr_head(12).is_reserved());
    }
}
