// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! The inventory of a diff (D-54 item 1): every file in exactly one class,
//! and every hunk of a file with content in one class where that can be
//! decided, deterministically from the diff itself. Classes other than
//! changed content are folded in diff views and always listed with their
//! reason, so nothing leaves a review without saying so.

use std::collections::HashSet;

use crate::diff::{Diff, FileDiff, Hunk, LineKind, Status};

/// What a file's change is. The order of the checks decides a file that
/// would fit several: a file cut at a limit is "not shown" whatever it is,
/// then the entry kind (submodule, symlink, binary), then generated, then
/// the kind of change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FileClass {
    /// Text changes worth reading.
    Content,
    /// Renamed without changes.
    Rename,
    /// Copied without changes.
    Copy,
    /// Only the file mode changed.
    ModeOnly,
    /// Only whitespace changed.
    WhitespaceOnly,
    /// Marked generated (`linguist-generated` or `slath-generated`).
    Generated,
    Binary,
    Symlink,
    Submodule,
    /// Dropped at a limit (named in the inventory).
    NotShown,
}

impl FileClass {
    /// The name used in the API and the UI.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Content => "content",
            Self::Rename => "rename",
            Self::Copy => "copy",
            Self::ModeOnly => "mode_only",
            Self::WhitespaceOnly => "whitespace_only",
            Self::Generated => "generated",
            Self::Binary => "binary",
            Self::Symlink => "symlink",
            Self::Submodule => "submodule",
            Self::NotShown => "not_shown",
        }
    }

    /// Folded in diff views (everything but changed content).
    #[must_use]
    pub fn folded(self) -> bool {
        self != Self::Content
    }
}

/// What a hunk of a file with content changes is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HunkClass {
    Content,
    WhitespaceOnly,
}

impl HunkClass {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Content => "content",
            Self::WhitespaceOnly => "whitespace_only",
        }
    }
}

/// One file of the inventory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileEntry {
    /// The path shown (the new path, except for deletions).
    pub path: Vec<u8>,
    pub old_path: Option<Vec<u8>>,
    pub class: FileClass,
    /// For content files, each hunk's class; empty otherwise.
    pub hunks: Vec<HunkClass>,
    /// Why it's in its class, in words.
    pub reason: String,
}

/// The inventory of a diff.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Inventory {
    pub files: Vec<FileEntry>,
    /// Files past the file limit: in the diff, not in the list.
    pub left_out: usize,
    /// The limit files were left out at, if any (`diff_max_files`).
    pub files_limit: Option<&'static str>,
}

/// Whether a hunk changes nothing but whitespace: its removed and added
/// lines are the same once all whitespace is gone.
fn whitespace_only(h: &Hunk) -> bool {
    let squash = |kind: LineKind| -> Vec<u8> {
        h.lines
            .iter()
            .filter(|l| l.kind == kind)
            .flat_map(|l| l.text.iter().copied())
            .filter(|b| !b.is_ascii_whitespace())
            .collect()
    };
    let changed = h
        .lines
        .iter()
        .any(|l| matches!(l.kind, LineKind::Added | LineKind::Removed));
    changed && squash(LineKind::Removed) == squash(LineKind::Added)
}

fn mode_is(f: &FileDiff, mode: &str) -> bool {
    [&f.old_mode, &f.new_mode, &f.index_mode]
        .iter()
        .any(|m| m.as_deref() == Some(mode))
}

fn classify(f: &FileDiff, generated: &HashSet<Vec<u8>>) -> (FileClass, Vec<HunkClass>, String) {
    if f.cut {
        return (
            FileClass::NotShown,
            Vec::new(),
            "over the diff line limit (diff_max_lines)".to_owned(),
        );
    }
    if mode_is(f, "160000") {
        return (FileClass::Submodule, Vec::new(), "a submodule".to_owned());
    }
    if mode_is(f, "120000") {
        return (FileClass::Symlink, Vec::new(), "a symbolic link".to_owned());
    }
    if f.binary {
        return (FileClass::Binary, Vec::new(), "a binary file".to_owned());
    }
    if generated.contains(f.path()) {
        return (
            FileClass::Generated,
            Vec::new(),
            "marked generated in .gitattributes".to_owned(),
        );
    }
    if f.hunks.is_empty() {
        return match f.status {
            Status::Renamed => (
                FileClass::Rename,
                Vec::new(),
                "renamed, unchanged".to_owned(),
            ),
            Status::Copied => (FileClass::Copy, Vec::new(), "copied, unchanged".to_owned()),
            _ if f.old_mode.is_some() && f.new_mode.is_some() && f.old_mode != f.new_mode => (
                FileClass::ModeOnly,
                Vec::new(),
                format!(
                    "only the mode changed ({} to {})",
                    f.old_mode.as_deref().unwrap_or(""),
                    f.new_mode.as_deref().unwrap_or("")
                ),
            ),
            // An empty file added or deleted.
            _ => (FileClass::Content, Vec::new(), "changed".to_owned()),
        };
    }
    let hunks: Vec<HunkClass> = f
        .hunks
        .iter()
        .map(|h| {
            if whitespace_only(h) {
                HunkClass::WhitespaceOnly
            } else {
                HunkClass::Content
            }
        })
        .collect();
    if hunks.iter().all(|h| *h == HunkClass::WhitespaceOnly) {
        (
            FileClass::WhitespaceOnly,
            hunks,
            "only whitespace changed".to_owned(),
        )
    } else {
        (FileClass::Content, hunks, "changed".to_owned())
    }
}

/// The inventory of `diff` (parsed with `max_files`), with `total_files`
/// files in the whole diff and `generated` the paths marked generated.
#[must_use]
pub fn inventory(
    diff: &Diff,
    total_files: usize,
    max_files: usize,
    generated: &HashSet<Vec<u8>>,
) -> Inventory {
    let files: Vec<FileEntry> = diff
        .files
        .iter()
        .map(|f| {
            let (class, hunks, reason) = classify(f, generated);
            FileEntry {
                path: f.path().to_vec(),
                old_path: matches!(f.status, Status::Renamed | Status::Copied)
                    .then(|| f.old_path.clone()),
                class,
                hunks,
                reason,
            }
        })
        .collect();
    let left_out = total_files.saturating_sub(files.len());
    Inventory {
        files,
        left_out,
        files_limit: (left_out > 0 && diff.files.len() >= max_files).then_some("diff_max_files"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diff::parse;

    const DIFF: &[u8] = b"diff --git a/a.txt b/a.txt
index 1111111..2222222 100644
--- a/a.txt
+++ b/a.txt
@@ -1,2 +1,2 @@
-one
+ONE
 two
@@ -9,1 +9,1 @@
-  x = 1;
+\tx = 1;
diff --git a/ws.txt b/ws.txt
index 1111111..2222222 100644
--- a/ws.txt
+++ b/ws.txt
@@ -1 +1 @@
-a  b
+a b
diff --git a/old.txt b/new.txt
similarity index 100%
rename from old.txt
rename to new.txt
diff --git a/run.sh b/run.sh
old mode 100644
new mode 100755
diff --git a/gen.txt b/gen.txt
index 1111111..2222222 100644
--- a/gen.txt
+++ b/gen.txt
@@ -1 +1 @@
-g
+G
diff --git a/bin.dat b/bin.dat
index 1111111..2222222 100644
Binary files a/bin.dat and b/bin.dat differ
diff --git a/link b/link
index 1111111..2222222 120000
--- a/link
+++ b/link
@@ -1 +1 @@
-a.txt
\\ No newline at end of file
+ws.txt
\\ No newline at end of file
diff --git a/vendor/lib b/vendor/lib
index 1111111..2222222 160000
--- a/vendor/lib
+++ b/vendor/lib
@@ -1 +1 @@
-Subproject commit 1111111111111111111111111111111111111111
+Subproject commit 2222222222222222222222222222222222222222
";

    #[test]
    fn every_file_in_one_class() {
        let generated: HashSet<Vec<u8>> = [b"gen.txt".to_vec()].into();
        let d = parse(DIFF, 100, 1000);
        let inv = inventory(&d, 8, 100, &generated);
        let classes: Vec<(&str, &str)> = inv
            .files
            .iter()
            .map(|f| (std::str::from_utf8(&f.path).unwrap(), f.class.as_str()))
            .collect();
        assert_eq!(
            classes,
            [
                ("a.txt", "content"),
                ("ws.txt", "whitespace_only"),
                ("new.txt", "rename"),
                ("run.sh", "mode_only"),
                ("gen.txt", "generated"),
                ("bin.dat", "binary"),
                ("link", "symlink"),
                ("vendor/lib", "submodule"),
            ]
        );
        assert_eq!(
            inv.files[0].hunks,
            [HunkClass::Content, HunkClass::WhitespaceOnly]
        );
        assert_eq!(inv.left_out, 0);
    }

    #[test]
    fn limits_are_named() {
        let generated = HashSet::new();
        // Two files fit, the rest is left out; the second is cut at the
        // line limit.
        let d = parse(DIFF, 2, 5);
        let inv = inventory(&d, 8, 2, &generated);
        assert_eq!(inv.files.len(), 2);
        assert_eq!(inv.files[0].class, FileClass::Content);
        assert_eq!(inv.files[1].class, FileClass::NotShown);
        assert!(inv.files[1].reason.contains("diff_max_lines"));
        assert_eq!(inv.left_out, 6);
        assert_eq!(inv.files_limit, Some("diff_max_files"));
    }
}
