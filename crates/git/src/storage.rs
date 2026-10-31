// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Repository storage on disk.
//!
//! Paths come only from repository IDs, never from user-visible names.

use std::path::{Path, PathBuf};

use slath_core::ids::RepoId;

use crate::args::RefName;
use crate::error::GitError;
use crate::runner::Runner;

/// Per-repository config written at creation. Ref policy lives in the hooks,
/// so Git itself allows non-fast-forwards and deletions.
pub const REPO_CONFIG: &[(&str, &str)] = &[
    ("core.logAllRefUpdates", "true"),
    ("receive.fsckObjects", "true"),
    ("receive.denyNonFastForwards", "false"),
    ("receive.denyDeletes", "false"),
    ("transfer.hideRefs", "refs/slath"),
    ("gc.auto", "0"),
    ("uploadpack.allowFilter", "true"),
    ("uploadpack.allowAnySHA1InWant", "false"),
];

/// `<data_dir>/repos/<id[0..2]>/<id>.git`.
#[must_use]
pub fn repo_path(data_dir: &Path, id: RepoId) -> PathBuf {
    let id = id.to_string();
    data_dir
        .join("repos")
        .join(&id[..2])
        .join(format!("{id}.git"))
}

/// Creates a bare repository for `id`. It's initialized under
/// `<data_dir>/repos/.tmp/` and renamed into place, so a crash never leaves a
/// half-initialized repository at the final path. Fails if it already exists.
pub async fn create_bare(
    runner: &Runner,
    data_dir: &Path,
    id: RepoId,
    default_branch: &str,
) -> Result<PathBuf, GitError> {
    let branch = RefName::branch(default_branch.as_bytes()).map_err(GitError::InvalidArgument)?;
    let final_path = repo_path(data_dir, id);
    if final_path.exists() {
        return Err(GitError::InvalidArgument("repository already exists"));
    }
    let tmp_root = data_dir.join("repos").join(".tmp");
    tokio::fs::create_dir_all(&tmp_root).await?;
    let tmp = tmp_root.join(format!("{id}.git"));
    if tmp.exists() {
        // Leftover from a crash during an earlier attempt.
        tokio::fs::remove_dir_all(&tmp).await?;
    }
    let mut initial = std::ffi::OsString::from("--initial-branch=");
    initial.push(std::ffi::OsStr::new(
        std::str::from_utf8(&branch.as_bytes()["refs/heads/".len()..])
            .map_err(|_| GitError::InvalidArgument("default branch must be UTF-8"))?,
    ));
    runner
        .repo(&tmp)
        .args(["init", "--bare", "--quiet"])
        .arg(initial)
        .run()
        .await?;
    for (key, value) in REPO_CONFIG {
        runner
            .repo(&tmp)
            .args(["config", "set", key, value])
            .run()
            .await?;
    }
    if let Some(parent) = final_path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    tokio::fs::rename(&tmp, &final_path).await?;
    Ok(final_path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_derive_from_ids_only() {
        let id: RepoId = "0192f1a2-3b4c-7d5e-8f60-718293a4b5c6".parse().unwrap();
        assert_eq!(
            repo_path(Path::new("/data"), id),
            Path::new("/data/repos/01/0192f1a2-3b4c-7d5e-8f60-718293a4b5c6.git")
        );
    }
}
