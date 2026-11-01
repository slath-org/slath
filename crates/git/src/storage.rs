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
    // Every object entering the repository is checked: pushes and the
    // internal fetches for forks and merge requests alike.
    ("transfer.fsckObjects", "true"),
    // Push options could become a side channel around policy; none are used.
    ("receive.advertisePushOptions", "false"),
    ("receive.denyNonFastForwards", "false"),
    ("receive.denyDeletes", "false"),
    ("transfer.hideRefs", "refs/slath"),
    ("gc.auto", "0"),
    ("uploadpack.allowFilter", "true"),
    ("uploadpack.allowAnySHA1InWant", "false"),
];

/// Version of [`REPO_CONFIG`]. Bump it when the config changes: a
/// background migration named after it re-applies the config to every
/// existing repository (`slath.configVersion` records what a repository has).
pub const REPO_CONFIG_VERSION: u32 = 1;

/// Writes [`REPO_CONFIG`] and the config version into a repository unless
/// it already has the current version. Returns whether it wrote anything.
pub async fn apply_config(runner: &Runner, git_dir: &Path) -> Result<bool, GitError> {
    let current = runner
        .repo(git_dir)
        .args(["config", "get", "slath.configVersion"])
        .output()
        .await
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok());
    if current.as_deref().map(str::trim) == Some(REPO_CONFIG_VERSION.to_string().as_str()) {
        return Ok(false);
    }
    for (key, value) in REPO_CONFIG {
        runner
            .repo(git_dir)
            .args(["config", "set", key, value])
            .run()
            .await?;
    }
    runner
        .repo(git_dir)
        .args(["config", "set", "slath.configVersion"])
        .arg(REPO_CONFIG_VERSION.to_string())
        .run()
        .await?;
    Ok(true)
}

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
    apply_config(runner, &tmp).await?;
    if let Some(parent) = final_path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    tokio::fs::rename(&tmp, &final_path).await?;
    Ok(final_path)
}

/// True if the repository has no branches or tags yet.
pub async fn is_empty(runner: &Runner, git_dir: &Path) -> Result<bool, GitError> {
    let out = runner
        .repo(git_dir)
        .args([
            "for-each-ref",
            "--count=1",
            "--format=%(refname)",
            "refs/heads",
            "refs/tags",
        ])
        .run()
        .await?;
    Ok(out.stdout.is_empty())
}

/// Points `HEAD` at `full_ref` (e.g. `refs/heads/main`) after validating it.
pub async fn set_head(runner: &Runner, git_dir: &Path, full_ref: &str) -> Result<(), GitError> {
    let r = RefName::parse(full_ref.as_bytes().to_vec()).map_err(GitError::InvalidArgument)?;
    runner
        .repo(git_dir)
        .args(["symbolic-ref", "HEAD"])
        .arg_bytes(r.as_bytes())
        .run()
        .await?;
    Ok(())
}

/// Repacks and prunes like `git gc`. Objects reachable from any ref,
/// including Slath's internal `refs/slath/*`, are kept; unreachable ones
/// only after two weeks.
pub async fn maintain(
    runner: &Runner,
    git_dir: &Path,
    timeout: std::time::Duration,
) -> Result<(), GitError> {
    runner
        .repo(git_dir)
        .config("gc.pruneExpire", "2.weeks.ago")
        .args(["maintenance", "run", "--task=gc", "--quiet"])
        .timeout(timeout)
        .run()
        .await?;
    Ok(())
}

/// Bytes on disk used by objects (`git count-objects -v`: loose, packed,
/// and garbage).
pub async fn disk_usage(runner: &Runner, git_dir: &Path) -> Result<u64, GitError> {
    let out = runner
        .repo(git_dir)
        .args(["count-objects", "-v"])
        .run()
        .await?;
    Ok(parse_count_objects(&out.stdout))
}

fn parse_count_objects(out: &[u8]) -> u64 {
    String::from_utf8_lossy(out)
        .lines()
        .filter_map(|l| l.split_once(": "))
        .filter(|(k, _)| matches!(*k, "size" | "size-pack" | "size-garbage"))
        .filter_map(|(_, v)| v.trim().parse::<u64>().ok())
        .sum::<u64>()
        .saturating_mul(1024)
}

/// Checks that every object reachable from the refs is present
/// (`git fsck --connectivity-only`).
pub async fn check_connectivity(runner: &Runner, git_dir: &Path) -> Result<(), GitError> {
    runner
        .repo(git_dir)
        .args([
            "fsck",
            "--connectivity-only",
            "--no-dangling",
            "--no-progress",
        ])
        .run()
        .await?;
    Ok(())
}

/// True if commit `old` is an ancestor of `new` (a fast-forward), with the
/// objects of a quarantined push when `quarantine` is given.
pub async fn is_ancestor(
    runner: &Runner,
    git_dir: &Path,
    old: &crate::object::ObjectId,
    new: &crate::object::ObjectId,
    quarantine: Option<&Path>,
) -> Result<bool, GitError> {
    let mut inv = runner.repo(git_dir);
    if let Some(q) = quarantine {
        inv = inv.quarantine(git_dir, q)?;
    }
    let out = inv
        .args(["merge-base", "--is-ancestor", "--end-of-options"])
        .arg(old.as_str())
        .arg(new.as_str())
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn count_objects_sizes_are_summed_in_bytes() {
        let out = b"count: 3\nsize: 12\nin-pack: 40\npacks: 1\nsize-pack: 100\nprune-packable: 0\ngarbage: 0\nsize-garbage: 2\n";
        assert_eq!(parse_count_objects(out), 114 * 1024);
    }

    #[test]
    fn paths_derive_from_ids_only() {
        let id: RepoId = "0192f1a2-3b4c-7d5e-8f60-718293a4b5c6".parse().unwrap();
        assert_eq!(
            repo_path(Path::new("/data"), id),
            Path::new("/data/repos/01/0192f1a2-3b4c-7d5e-8f60-718293a4b5c6.git")
        );
    }
}
