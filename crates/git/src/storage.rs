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

/// Whether a directory tree holds only directories and regular files: no
/// symlinks or other special files, which a local clone would follow or
/// copy into another repository.
fn only_plain_files(dir: &Path) -> std::io::Result<bool> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let t = entry.file_type()?;
        if t.is_dir() {
            if !only_plain_files(&entry.path())? {
                return Ok(false);
            }
        } else if !t.is_file() {
            return Ok(false);
        }
    }
    Ok(true)
}

/// Hardlinks (or copies, across filesystems) the parent's regular packs
/// into `to`: each pack's index first, then the pack, so a pack that
/// vanishes meanwhile (a concurrent gc) is skipped whole. Cruft packs (a
/// `.mtimes` file) hold only unreachable objects and aren't linked; loose
/// objects aren't either.
fn link_packs(from: &Path, to: &Path) -> std::io::Result<()> {
    let Ok(entries) = std::fs::read_dir(from) else {
        return Ok(());
    };
    std::fs::create_dir_all(to)?;
    for entry in entries {
        let path = entry?.path();
        if path.extension().is_none_or(|e| e != "pack") || !path.is_file() {
            continue;
        }
        if path.with_extension("mtimes").exists() || path.with_extension("promisor").exists() {
            continue;
        }
        let mut linked = Vec::new();
        let mut ok = true;
        for ext in ["idx", "rev", "bitmap", "pack"] {
            let src = path.with_extension(ext);
            if ext != "idx" && ext != "pack" && !src.exists() {
                continue;
            }
            let Some(name) = src.file_name() else {
                ok = false;
                break;
            };
            let dst = to.join(name);
            if std::fs::hard_link(&src, &dst)
                .or_else(|_| std::fs::copy(&src, &dst).map(|_| ()))
                .is_err()
            {
                ok = false;
                break;
            }
            linked.push(dst);
        }
        if !ok {
            for f in linked {
                let _ = std::fs::remove_file(f);
            }
        }
    }
    Ok(())
}

/// Replaces the (empty) repository of `id` with a copy of `parent`'s
/// branches and tags (or only `branch`, with the tags in its history).
///
/// Git serves any object a repository holds to whoever asks for it by ID
/// (protocol v2 has no reachability check), so a fork must not hold
/// objects only the parent's deleted refs reached. The parent's regular
/// packs are hardlinked (copied across filesystems); its loose objects and
/// cruft packs, where unreachable objects end up, aren't. A local fetch
/// then brings what's missing, sending only objects reachable from the
/// copied refs. Unreachable objects inside a regular pack (from a recent
/// push whose branch was deleted before the parent's next gc) remain
/// possible. Internal and merge request refs never come along. The copy is
/// made under `<data_dir>/repos/.tmp/` and swapped into place.
pub async fn fork_clone(
    runner: &Runner,
    data_dir: &Path,
    parent: &Path,
    id: RepoId,
    branch: Option<&str>,
) -> Result<PathBuf, GitError> {
    let objects = parent.join("objects");
    let plain = tokio::task::spawn_blocking(move || only_plain_files(&objects))
        .await
        .map_err(|e| GitError::Io(std::io::Error::other(e)))??;
    if !plain {
        return Err(GitError::InvalidArgument(
            "the parent's object store holds something other than plain files",
        ));
    }
    let branch = branch
        .map(|b| RefName::branch(b.as_bytes()).map_err(GitError::InvalidArgument))
        .transpose()?;
    let final_path = repo_path(data_dir, id);
    let tmp_root = data_dir.join("repos").join(".tmp");
    tokio::fs::create_dir_all(&tmp_root).await?;
    let tmp = tmp_root.join(format!("{id}.fork.git"));
    let old = tmp_root.join(format!("{id}.empty.git"));
    for leftover in [&tmp, &old] {
        if leftover.exists() {
            tokio::fs::remove_dir_all(leftover).await?;
        }
    }
    runner
        .repo(&tmp)
        .args(["init", "--bare", "--quiet"])
        .run()
        .await?;
    apply_config(runner, &tmp).await?;
    let (from, to) = (parent.join("objects/pack"), tmp.join("objects/pack"));
    tokio::task::spawn_blocking(move || link_packs(&from, &to))
        .await
        .map_err(|e| GitError::Io(std::io::Error::other(e)))??;

    // Refs whose history the linked packs already hold completely are
    // written directly; the fetch below then only negotiates the rest.
    let wanted: Vec<&[u8]> = match &branch {
        Some(b) => vec![b.as_bytes()],
        None => vec![b"refs/heads", b"refs/tags"],
    };
    let mut list = runner
        .repo(parent)
        .args(["for-each-ref", "--format=%(objectname) %(refname)"]);
    for w in &wanted {
        list = list.arg_bytes(w);
    }
    let listed = list.run().await?.stdout;
    let tips: Vec<(&[u8], &[u8])> = listed
        .split(|c| *c == b'\n')
        .filter_map(|l| {
            let sp = l.iter().position(|c| *c == b' ')?;
            Some((&l[..sp], &l[sp + 1..]))
        })
        .collect();
    if !tips.is_empty() {
        let mut stdin = Vec::new();
        for (oid, _) in &tips {
            stdin.extend_from_slice(oid);
            stdin.push(b'\n');
        }
        let complete = runner
            .repo(&tmp)
            .args([
                "rev-list",
                "--objects",
                "--quiet",
                "--use-bitmap-index",
                "--stdin",
            ])
            .stdin(stdin)
            .timeout(std::time::Duration::from_secs(3600))
            .output()
            .await?
            .status
            .success();
        if complete {
            let mut script = Vec::new();
            for (oid, name) in &tips {
                script.extend_from_slice(b"create ");
                script.extend_from_slice(name);
                script.push(b' ');
                script.extend_from_slice(oid);
                script.push(b'\n');
            }
            runner
                .repo(&tmp)
                .args(["update-ref", "--stdin"])
                .stdin(script)
                .run()
                .await?;
        }
    }
    let mut fetch = runner
        .repo(&tmp)
        .config("protocol.file.allow", "always")
        .args(["fetch", "--quiet", "--no-write-fetch-head", "--"])
        .arg(parent);
    match &branch {
        Some(b) => {
            let mut spec = b"+".to_vec();
            spec.extend_from_slice(b.as_bytes());
            spec.push(b':');
            spec.extend_from_slice(b.as_bytes());
            fetch = fetch.arg_bytes(&spec);
        }
        None => {
            fetch = fetch.args(["+refs/heads/*:refs/heads/*", "+refs/tags/*:refs/tags/*"]);
        }
    }
    fetch
        .timeout(std::time::Duration::from_secs(3600))
        .run()
        .await?;
    if final_path.exists() {
        tokio::fs::rename(&final_path, &old).await?;
    } else if let Some(p) = final_path.parent() {
        tokio::fs::create_dir_all(p).await?;
    }
    tokio::fs::rename(&tmp, &final_path).await?;
    if old.exists() {
        tokio::fs::remove_dir_all(&old).await?;
    }
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

/// The full ID of the commit an abbreviated (or full) hex ID names, if
/// exactly one commit matches.
pub async fn resolve_commit_prefix(
    runner: &Runner,
    git_dir: &Path,
    prefix: &str,
) -> Result<Option<String>, GitError> {
    if !(4..=40).contains(&prefix.len()) || !prefix.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Ok(None);
    }
    let out = runner
        .repo(git_dir)
        .args(["rev-parse", "--verify", "--quiet", "--end-of-options"])
        .arg(format!("{}^{{commit}}", prefix.to_ascii_lowercase()))
        .output()
        .await?;
    if !out.status.success() {
        return Ok(None);
    }
    let full = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    Ok((full.len() == 40 && full.bytes().all(|b| b.is_ascii_hexdigit())).then_some(full))
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
