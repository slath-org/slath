// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! A parser for `git diff --patch` output, bounded by a file and a line
//! limit. Content past the limits is dropped and reported as truncated.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Added,
    Deleted,
    Modified,
    Renamed,
    Copied,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineKind {
    Context,
    Added,
    Removed,
    /// "\ No newline at end of file"
    NoNewline,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Line {
    pub kind: LineKind,
    pub old_no: Option<u32>,
    pub new_no: Option<u32>,
    pub text: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hunk {
    pub old_start: u32,
    pub new_start: u32,
    /// Text after the second `@@` (often the enclosing function).
    pub section: Vec<u8>,
    pub lines: Vec<Line>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileDiff {
    pub old_path: Vec<u8>,
    pub new_path: Vec<u8>,
    pub status: Status,
    pub binary: bool,
    pub old_mode: Option<String>,
    pub new_mode: Option<String>,
    pub hunks: Vec<Hunk>,
    pub additions: u32,
    pub deletions: u32,
}

impl FileDiff {
    /// The path to show (the new path, except for deletions).
    #[must_use]
    pub fn path(&self) -> &[u8] {
        if self.status == Status::Deleted {
            &self.old_path
        } else {
            &self.new_path
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Diff {
    pub files: Vec<FileDiff>,
    /// Files or lines were dropped because of the limits.
    pub truncated: bool,
}

/// Undoes Git's C-style path quoting (`"a\tb"`, octal escapes).
fn unquote(s: &[u8]) -> Vec<u8> {
    let Some(inner) = s.strip_prefix(b"\"").and_then(|r| r.strip_suffix(b"\"")) else {
        return s.to_vec();
    };
    let mut out = Vec::with_capacity(inner.len());
    let mut i = 0;
    while i < inner.len() {
        if inner[i] == b'\\' && i + 1 < inner.len() {
            let c = inner[i + 1];
            let (byte, used) = match c {
                b'n' => (b'\n', 2),
                b't' => (b'\t', 2),
                b'r' => (b'\r', 2),
                b'a' => (7, 2),
                b'b' => (8, 2),
                b'f' => (12, 2),
                b'v' => (11, 2),
                b'0'..=b'3' if inner.len() >= i + 4 => {
                    let oct = &inner[i + 1..i + 4];
                    match std::str::from_utf8(oct)
                        .ok()
                        .and_then(|o| u8::from_str_radix(o, 8).ok())
                    {
                        Some(v) => (v, 4),
                        None => (c, 2),
                    }
                }
                other => (other, 2),
            };
            out.push(byte);
            i += used;
        } else {
            out.push(inner[i]);
            i += 1;
        }
    }
    out
}

/// A path from a `---`/`+++` line: `a/x`, `b/x`, or `/dev/null`.
fn side_path(rest: &[u8]) -> Option<Vec<u8>> {
    let rest = rest.strip_suffix(b"\t").unwrap_or(rest);
    if rest == b"/dev/null" {
        return None;
    }
    let p = unquote(rest);
    Some(p.get(2..).map(<[u8]>::to_vec).unwrap_or_default())
}

/// Paths from `diff --git a/X b/X` when both are equal and unquoted.
fn header_paths(rest: &[u8]) -> (Vec<u8>, Vec<u8>) {
    if rest.starts_with(b"\"") {
        // "a/x" "b/y"
        if let Some(end) = rest[1..].iter().position(|&b| b == b'"').map(|e| e + 2) {
            let a = unquote(&rest[..end]);
            let b = unquote(rest[end..].trim_ascii_start());
            return (
                a.get(2..).unwrap_or_default().to_vec(),
                b.get(2..).unwrap_or_default().to_vec(),
            );
        }
    }
    if rest.len() >= 5 && (rest.len() - 5).is_multiple_of(2) {
        let p = (rest.len() - 5) / 2;
        let (a, b) = (&rest[2..2 + p], &rest[5 + p..]);
        if rest.starts_with(b"a/") && &rest[2 + p..5 + p] == b" b/" && a == b {
            return (a.to_vec(), b.to_vec());
        }
    }
    (Vec::new(), Vec::new())
}

fn parse_range(s: &[u8]) -> u32 {
    let s = std::str::from_utf8(s).unwrap_or("0");
    s.split(',')
        .next()
        .and_then(|n| n.parse().ok())
        .unwrap_or(0)
}

/// Parses `git diff --patch` output, keeping at most `max_files` files and
/// `max_lines` diff lines in total.
#[must_use]
pub fn parse(out: &[u8], max_files: usize, max_lines: usize) -> Diff {
    let mut diff = Diff::default();
    let mut lines_left = max_lines;
    let mut cur: Option<FileDiff> = None;
    let (mut old_no, mut new_no) = (0u32, 0u32);
    let mut skipping = false;
    for raw in out.split(|&b| b == b'\n') {
        if let Some(rest) = raw.strip_prefix(b"diff --git ") {
            if let Some(f) = cur.take() {
                diff.files.push(f);
            }
            if diff.files.len() >= max_files {
                diff.truncated = true;
                break;
            }
            skipping = false;
            let (a, b) = header_paths(rest);
            cur = Some(FileDiff {
                old_path: a,
                new_path: b,
                status: Status::Modified,
                binary: false,
                old_mode: None,
                new_mode: None,
                hunks: Vec::new(),
                additions: 0,
                deletions: 0,
            });
            continue;
        }
        let Some(f) = cur.as_mut() else { continue };
        if let Some(rest) = raw.strip_prefix(b"@@ ") {
            // @@ -a,b +c,d @@ section
            let mut parts = rest.splitn(3, |&b| b == b' ');
            let old = parts.next().unwrap_or_default();
            let new = parts.next().unwrap_or_default();
            let tail = parts.next().unwrap_or_default();
            let section = tail
                .strip_prefix(b"@@")
                .unwrap_or(tail)
                .trim_ascii()
                .to_vec();
            old_no = parse_range(old.strip_prefix(b"-").unwrap_or(old));
            new_no = parse_range(new.strip_prefix(b"+").unwrap_or(new));
            f.hunks.push(Hunk {
                old_start: old_no,
                new_start: new_no,
                section,
                lines: Vec::new(),
            });
            continue;
        }
        if f.hunks.is_empty() {
            // Extended header lines.
            if raw.starts_with(b"new file mode ") {
                f.status = Status::Added;
                f.new_mode = Some(String::from_utf8_lossy(&raw[14..]).into_owned());
            } else if raw.starts_with(b"deleted file mode ") {
                f.status = Status::Deleted;
                f.old_mode = Some(String::from_utf8_lossy(&raw[18..]).into_owned());
            } else if let Some(m) = raw.strip_prefix(b"old mode ") {
                f.old_mode = Some(String::from_utf8_lossy(m).into_owned());
            } else if let Some(m) = raw.strip_prefix(b"new mode ") {
                f.new_mode = Some(String::from_utf8_lossy(m).into_owned());
            } else if let Some(p) = raw.strip_prefix(b"rename from ") {
                f.status = Status::Renamed;
                f.old_path = unquote(p);
            } else if let Some(p) = raw.strip_prefix(b"rename to ") {
                f.new_path = unquote(p);
            } else if let Some(p) = raw.strip_prefix(b"copy from ") {
                f.status = Status::Copied;
                f.old_path = unquote(p);
            } else if let Some(p) = raw.strip_prefix(b"copy to ") {
                f.new_path = unquote(p);
            } else if let Some(p) = raw.strip_prefix(b"--- ") {
                if let Some(p) = side_path(p) {
                    f.old_path = p;
                }
            } else if let Some(p) = raw.strip_prefix(b"+++ ") {
                if let Some(p) = side_path(p) {
                    f.new_path = p;
                }
            } else if raw.starts_with(b"Binary files ") || raw == b"GIT binary patch" {
                f.binary = true;
            }
            continue;
        }
        if skipping {
            continue;
        }
        let (kind, text) = match raw.first() {
            Some(b'+') => (LineKind::Added, &raw[1..]),
            Some(b'-') => (LineKind::Removed, &raw[1..]),
            Some(b' ') => (LineKind::Context, &raw[1..]),
            Some(b'\\') => (LineKind::NoNewline, raw),
            _ => continue,
        };
        if lines_left == 0 {
            diff.truncated = true;
            skipping = true;
            continue;
        }
        lines_left -= 1;
        let (o, n) = match kind {
            LineKind::Added => {
                f.additions += 1;
                new_no += 1;
                (None, Some(new_no - 1))
            }
            LineKind::Removed => {
                f.deletions += 1;
                old_no += 1;
                (Some(old_no - 1), None)
            }
            LineKind::Context => {
                old_no += 1;
                new_no += 1;
                (Some(old_no - 1), Some(new_no - 1))
            }
            LineKind::NoNewline => (None, None),
        };
        if let Some(h) = f.hunks.last_mut() {
            h.lines.push(Line {
                kind,
                old_no: o,
                new_no: n,
                text: text.to_vec(),
            });
        }
    }
    if let Some(f) = cur.take() {
        diff.files.push(f);
    }
    diff
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &[u8] = b"diff --git a/src/a.rs b/src/a.rs
index 1111111111111111111111111111111111111111..2222222222222222222222222222222222222222 100644
--- a/src/a.rs
+++ b/src/a.rs
@@ -1,3 +1,4 @@ fn main() {
 one
-two
+TWO
+three
 four
diff --git a/new file.txt b/new file.txt
new file mode 100644
index 0000000000000000000000000000000000000000..3333333333333333333333333333333333333333
--- /dev/null
+++ b/new file.txt
@@ -0,0 +1 @@
+hello
\\ No newline at end of file
diff --git a/old.txt b/renamed.txt
similarity index 100%
rename from old.txt
rename to renamed.txt
diff --git \"a/\\303\\244.bin\" \"b/\\303\\244.bin\"
index 4444444444444444444444444444444444444444..5555555555555555555555555555555555555555 100644
Binary files \"a/\\303\\244.bin\" and \"b/\\303\\244.bin\" differ
";

    #[test]
    fn parses_files_hunks_and_line_numbers() {
        let d = parse(SAMPLE, 100, 1000);
        assert!(!d.truncated);
        assert_eq!(d.files.len(), 4);
        let a = &d.files[0];
        assert_eq!(a.new_path, b"src/a.rs");
        assert_eq!((a.additions, a.deletions), (2, 1));
        let h = &a.hunks[0];
        assert_eq!(h.section, b"fn main() {");
        assert_eq!(
            h.lines[1],
            Line {
                kind: LineKind::Removed,
                old_no: Some(2),
                new_no: None,
                text: b"two".to_vec()
            }
        );
        assert_eq!(h.lines[2].new_no, Some(2));
        assert_eq!(h.lines[4].old_no, Some(3));
        assert_eq!(h.lines[4].new_no, Some(4));
        let n = &d.files[1];
        assert_eq!(n.status, Status::Added);
        assert_eq!(n.new_path, b"new file.txt");
        assert_eq!(n.hunks[0].lines[1].kind, LineKind::NoNewline);
        assert_eq!(d.files[2].status, Status::Renamed);
        assert_eq!(
            (
                d.files[2].old_path.as_slice(),
                d.files[2].new_path.as_slice()
            ),
            (&b"old.txt"[..], &b"renamed.txt"[..])
        );
        assert!(d.files[3].binary);
        assert_eq!(d.files[3].new_path, "\u{e4}.bin".as_bytes());
    }

    #[test]
    fn limits_truncate() {
        let d = parse(SAMPLE, 1, 1000);
        assert_eq!(d.files.len(), 1);
        assert!(d.truncated);
        let d = parse(SAMPLE, 100, 2);
        assert!(d.truncated);
        assert_eq!(d.files[0].hunks[0].lines.len(), 2);
    }
}
