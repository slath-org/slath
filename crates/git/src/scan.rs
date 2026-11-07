// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! What a push adds to a repository, for push protection: the commits not
//! reachable from any existing ref, and the blobs they introduce that the
//! repository doesn't have yet, each with the commit and path it first
//! appears in. Content the repository already has is never rescanned.

use std::collections::HashSet;
use std::path::Path;

use crate::error::GitError;
use crate::object::ObjectId;
use crate::runner::Runner;

/// A blob a push introduces.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewBlob {
    pub oid: ObjectId,
    /// The oldest new commit that adds it, and where.
    pub commit: ObjectId,
    pub path: Vec<u8>,
}

/// Everything new in a push.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NewContent {
    /// New commits, oldest first.
    pub commits: Vec<ObjectId>,
    pub blobs: Vec<NewBlob>,
    /// New annotated tag objects among the pushed tips.
    pub tags: Vec<ObjectId>,
}

fn oids(out: &[u8]) -> Result<Vec<ObjectId>, GitError> {
    out.split(|&b| b == b'\n')
        .filter(|l| !l.is_empty())
        .map(|l| ObjectId::parse(l).map_err(GitError::Protocol))
        .collect()
}

/// The new content of a push whose new ref values are `tips`, with its
/// objects in `quarantine` (pre-receive) or already in the repository.
/// With `everything`, all history reachable from `tips` counts as new
/// (an import's report-only scan).
pub async fn new_content(
    runner: &Runner,
    git_dir: &Path,
    quarantine: Option<&Path>,
    tips: &[ObjectId],
    everything: bool,
) -> Result<NewContent, GitError> {
    if tips.is_empty() {
        return Ok(NewContent::default());
    }
    let inv = || {
        let i = runner.repo(git_dir);
        match quarantine {
            Some(q) => i.quarantine(git_dir, q),
            None => Ok(i),
        }
    };
    let mut tip_args: Vec<&str> = tips.iter().map(ObjectId::as_str).collect();
    if !everything {
        tip_args.extend(["--not", "--all"]);
    }
    let objects = inv()?
        .args(["rev-list", "--objects", "--no-object-names"])
        .args(&tip_args)
        .run()
        .await?;
    let new: HashSet<ObjectId> = oids(&objects.stdout)?.into_iter().collect();
    let commits = inv()?
        .args(["rev-list", "--reverse"])
        .args(&tip_args)
        .run()
        .await?;
    let commits = oids(&commits.stdout)?;
    let mut blobs = Vec::new();
    let mut seen = HashSet::new();
    for c in &commits {
        let out = inv()?
            .args([
                "diff-tree",
                "-r",
                "-z",
                "--root",
                "-m",
                "--first-parent",
                "--no-commit-id",
                "--no-renames",
                "--diff-filter=AMT",
            ])
            .arg(c.as_str())
            .run()
            .await?;
        // Records: ":<mode> <mode> <old> <new> <status>\0<path>\0".
        let mut fields = out.stdout.split(|&b| b == 0);
        while let (Some(meta), Some(path)) = (fields.next(), fields.next()) {
            let meta = String::from_utf8_lossy(meta);
            let parts: Vec<&str> = meta.trim_start_matches(':').split(' ').collect();
            let [_, mode, _, oid, _] = parts[..] else {
                continue;
            };
            // Submodules point at commits, not content.
            if mode == "160000" {
                continue;
            }
            let Ok(oid) = ObjectId::parse(oid) else {
                continue;
            };
            if new.contains(&oid) && seen.insert(oid.clone()) {
                blobs.push(NewBlob {
                    oid,
                    commit: c.clone(),
                    path: path.to_vec(),
                });
            }
        }
    }
    let mut tags = Vec::new();
    for t in tips {
        if new.contains(t) && !commits.contains(t) {
            let out = inv()?
                .args(["cat-file", "-t"])
                .arg(t.as_str())
                .run()
                .await?;
            if out.stdout.trim_ascii() == b"tag" {
                tags.push(t.clone());
            }
        }
    }
    Ok(NewContent {
        commits,
        blobs,
        tags,
    })
}
