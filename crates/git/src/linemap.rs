// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Carrying review comments forward: where a line of one version of a file
//! is in a newer version, by the hunks of a zero-context diff between the
//! two commits (with rename detection). A line inside a changed hunk has no
//! position in the newer version.

use std::path::Path;

use crate::diff::{FileDiff, LineKind};
use crate::object::ObjectId;
use crate::{GitError, Runner};

/// Where line `n` (1-based) of the old side of `file` is on its new side,
/// or `None` if a hunk removed or replaced it.
#[must_use]
pub fn map_line(file: &FileDiff, n: u32) -> Option<u32> {
    let mut shift: i64 = 0;
    for h in &file.hunks {
        let removed = u32::try_from(
            h.lines
                .iter()
                .filter(|l| l.kind == LineKind::Removed)
                .count(),
        )
        .ok()?;
        let added =
            i64::try_from(h.lines.iter().filter(|l| l.kind == LineKind::Added).count()).ok()?;
        if removed == 0 {
            // A pure insertion after old line `old_start`.
            if n > h.old_start {
                shift += added;
            }
            continue;
        }
        let end = h.old_start + removed;
        if n < h.old_start {
            break;
        }
        if n < end {
            return None;
        }
        shift += added - i64::from(removed);
    }
    u32::try_from(i64::from(n) + shift).ok()
}

/// The zero-context diff from `a` to `b` (renames detected), parsed.
pub async fn diff_u0(
    runner: &Runner,
    git_dir: &Path,
    a: &ObjectId,
    b: &ObjectId,
) -> Result<crate::diff::Diff, GitError> {
    let out = runner
        .repo(git_dir)
        .config("core.quotePath", "false")
        .args([
            "diff",
            "--no-color",
            "--no-ext-diff",
            "--no-textconv",
            "-M",
            "-U0",
            "--end-of-options",
            a.as_str(),
            b.as_str(),
        ])
        .max_output(64 * 1024 * 1024)
        .run()
        .await?;
    Ok(crate::diff::parse(&out.stdout, usize::MAX, usize::MAX))
}

/// Where `path` line `n` of commit `a` is in commit `b`: the path there and
/// the line, or `None` if the line was changed or the file removed.
pub async fn carry(
    runner: &Runner,
    git_dir: &Path,
    (a, b): (&ObjectId, &ObjectId),
    path: &[u8],
    lines: (u32, u32),
) -> Result<Option<(Vec<u8>, u32, u32)>, GitError> {
    if a == b {
        return Ok(Some((path.to_vec(), lines.0, lines.1)));
    }
    let diff = diff_u0(runner, git_dir, a, b).await?;
    let Some(file) = diff.files.iter().find(|f| f.old_path == path) else {
        // Not in the diff: unchanged.
        return Ok(Some((path.to_vec(), lines.0, lines.1)));
    };
    if file.new_path.is_empty() || file.status == crate::diff::Status::Deleted {
        return Ok(None);
    }
    match (map_line(file, lines.0), map_line(file, lines.1)) {
        (Some(s), Some(e)) if e >= s && e - s == lines.1 - lines.0 => {
            Ok(Some((file.new_path.clone(), s, e)))
        }
        _ => Ok(None),
    }
}

/// Lines `start..=end` (1-based) of `path` at `commit`, or `None` if the
/// file or the lines don't exist there.
pub async fn read_lines(
    runner: &Runner,
    git_dir: &Path,
    commit: &ObjectId,
    path: &[u8],
    (start, end): (u32, u32),
) -> Result<Option<Vec<Vec<u8>>>, GitError> {
    let mut spec = commit.as_str().as_bytes().to_vec();
    spec.push(b':');
    spec.extend_from_slice(path);
    let out = runner
        .repo(git_dir)
        .args(["cat-file", "blob"])
        .arg_bytes(&spec)
        .max_output(64 * 1024 * 1024)
        .output()
        .await?;
    if !out.status.success() {
        return Ok(None);
    }
    let all: Vec<&[u8]> = out.stdout.split(|b| *b == b'\n').collect();
    let (s, e) = (start as usize, end as usize);
    if s == 0 || e < s || e > all.len() || (e == all.len() && out.stdout.ends_with(b"\n")) {
        return Ok(None);
    }
    Ok(Some(all[s - 1..e].iter().map(|l| l.to_vec()).collect()))
}

/// How many lines `path` has at `commit`, or `None` if it isn't a file
/// there.
pub async fn line_count(
    runner: &Runner,
    git_dir: &Path,
    commit: &ObjectId,
    path: &[u8],
) -> Result<Option<u32>, GitError> {
    let mut spec = commit.as_str().as_bytes().to_vec();
    spec.push(b':');
    spec.extend_from_slice(path);
    let out = runner
        .repo(git_dir)
        .args(["cat-file", "blob"])
        .arg_bytes(&spec)
        .max_output(64 * 1024 * 1024)
        .output()
        .await?;
    if !out.status.success() {
        return Ok(None);
    }
    Ok(Some(count_lines(&out.stdout)))
}

/// Lines as [`read_lines`] counts them: a final newline ends the last line.
fn count_lines(text: &[u8]) -> u32 {
    let n = text.iter().filter(|b| **b == b'\n').count()
        + usize::from(!text.is_empty() && !text.ends_with(b"\n"));
    u32::try_from(n).unwrap_or(u32::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_lines() {
        assert_eq!(count_lines(b""), 0);
        assert_eq!(count_lines(b"a"), 1);
        assert_eq!(count_lines(b"a\n"), 1);
        assert_eq!(count_lines(b"a\nb"), 2);
        assert_eq!(count_lines(b"a\n\n"), 2);
    }

    fn file(diff: &str) -> FileDiff {
        crate::diff::parse(diff.as_bytes(), 100, 10_000)
            .files
            .remove(0)
    }

    const HEAD: &str = "diff --git a/f b/f\nindex 1111111..2222222 100644\n--- a/f\n+++ b/f\n";

    #[test]
    fn lines_shift_around_changes() {
        // Line 3 replaced by two lines; one line inserted after line 7.
        let f = file(&format!(
            "{HEAD}@@ -3 +3,2 @@\n-c\n+C\n+C2\n@@ -7,0 +9 @@\n+new\n"
        ));
        assert_eq!(map_line(&f, 1), Some(1));
        assert_eq!(map_line(&f, 2), Some(2));
        assert_eq!(map_line(&f, 3), None);
        assert_eq!(map_line(&f, 4), Some(5));
        assert_eq!(map_line(&f, 7), Some(8));
        assert_eq!(map_line(&f, 8), Some(10));
    }

    #[test]
    fn removed_lines_have_no_position() {
        let f = file(&format!("{HEAD}@@ -2,2 +1,0 @@\n-b\n-c\n"));
        assert_eq!(map_line(&f, 1), Some(1));
        assert_eq!(map_line(&f, 2), None);
        assert_eq!(map_line(&f, 3), None);
        assert_eq!(map_line(&f, 4), Some(2));
    }
}
