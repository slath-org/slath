// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Diffs as every page shows them (commit, compare, merge request
//! changes): unified and split views, ignoring whitespace, a file tree,
//! collapsed generated files, and files that load as they come into view.

use std::collections::{HashMap, HashSet};

use askama::Template;
use axum::extract::State;
use axum::response::{IntoResponse, Redirect, Response};
use axum::{Form, Router};
use serde::Deserialize;
use slath_core::config::Limits;
use slath_git::diff::{LineKind, Status};
use slath_markup::intraline::{Bounds, CharacterRefiner, Refiner, Segment};

use super::PageCtx;
use super::browse::lossy;
use crate::AppState;
use crate::auth::RequireSession;
use crate::error::AppError;
use crate::routes::{Access, RouteExt};

/// The first files render with the page; later ones load as they scroll
/// into view.
const EAGER_FILES: usize = 20;
/// Files with more lines load only on request.
const CLICK_LINES: usize = 800;

pub struct LineView {
    /// `add`, `del`, `ctx`, or `meta`.
    pub kind: &'static str,
    pub old_no: String,
    pub new_no: String,
    pub text: String,
    /// The line in runs, changed ones marked; empty without emphasis.
    pub segments: Vec<Segment>,
}

/// One side of a split row; `kind` is `empty` for a blank side.
pub struct Cell {
    pub kind: &'static str,
    pub no: String,
    pub text: String,
    pub segments: Vec<Segment>,
}

pub struct SplitRow {
    pub left: Cell,
    pub right: Cell,
}

pub struct HunkView {
    pub head: String,
    pub lines: Vec<LineView>,
    pub rows: Vec<SplitRow>,
}

pub struct FileView {
    pub index: usize,
    pub anchor: String,
    pub path: String,
    pub old_path: Option<String>,
    pub status: &'static str,
    pub binary: bool,
    pub additions: u32,
    pub deletions: u32,
    /// Marked generated in `.gitattributes`: collapsed until asked for.
    pub generated: bool,
    /// Its class in the diff inventory, and why.
    pub class: &'static str,
    pub reason: String,
    /// Every class but changed content is folded until asked for.
    pub folded: bool,
    /// There are lines to load.
    pub has_lines: bool,
    /// The viewer marked it viewed (and it hasn't changed since).
    pub viewed: bool,
    /// `now`, `scroll` (loads when it comes into view), or `click`.
    pub load: &'static str,
    pub hunks: Vec<HunkView>,
}

/// An entry of the file tree: a directory or a file, indented by depth.
pub struct TreeEntry {
    pub depth: usize,
    pub name: String,
    /// The file's anchor; `None` for a directory.
    pub anchor: Option<String>,
    pub status: &'static str,
    /// A folded file, or a directory with only folded files: hidden while
    /// the list shows changed content only.
    pub folded: bool,
    /// Why a folded file is folded.
    pub reason: String,
}

/// A thread shown under its line.
pub struct InlineThread {
    pub id: String,
    pub resolved: bool,
    /// Carried from this earlier revision.
    pub from: Option<i32>,
    /// (author, rendered body, pending)
    pub comments: Vec<(String, super::MarkupHtml, bool)>,
    /// The thread in the conversation, for answering.
    pub link: String,
}

/// Review features on a merge request's diff.
pub struct ReviewCtx {
    /// Where new threads and viewed marks are posted.
    pub thread_post: String,
    pub viewed_post: String,
    /// The page to come back to.
    pub back: String,
    pub can_comment: bool,
    /// Threads by (path, side, last line).
    pub threads: HashMap<(String, String, String), Vec<InlineThread>>,
    /// The line being commented on: (path, side, line).
    pub composer: Option<(String, String, String)>,
    /// For a change's own diff: its commit.
    pub commit: Option<String>,
    pub revision: i32,
}

/// What the diff partial renders.
pub struct DiffView {
    pub files: Vec<FileView>,
    pub truncated: bool,
    pub split: bool,
    pub ignore_whitespace: bool,
    pub tree: Vec<TreeEntry>,
    /// The page's URL without the diff parameters.
    pub href: String,
    /// Only this file is shown.
    pub only: Option<usize>,
    /// The diff options are saved for signed-in users.
    pub signed_in: bool,
    /// Merge request diffs: threads, commenting, viewed files.
    pub review: Option<ReviewCtx>,
    /// Files with changed content, and folded files, in the list.
    pub content_files: usize,
    pub folded_files: usize,
    /// Files not listed, past the file limit, and that limit.
    pub left_out: usize,
    pub max_files: usize,
}

/// The side and number a line is anchored by: removed lines by their old
/// number, the rest by their new one.
fn side_of<'a>(kind: &str, old_no: &'a str, new_no: &'a str) -> (&'static str, &'a str) {
    if kind == "del" {
        ("old", old_no)
    } else {
        ("new", new_no)
    }
}

impl DiffView {
    /// `old` for removed lines, `new` otherwise.
    #[must_use]
    pub fn side_for(&self, kind: &str) -> &'static str {
        side_of(kind, "", "").0
    }

    /// The line number a line is commented by.
    #[must_use]
    pub fn line_for<'a>(&self, kind: &str, old_no: &'a str, new_no: &'a str) -> &'a str {
        side_of(kind, old_no, new_no).1
    }

    /// Threads that end on this line.
    #[must_use]
    pub fn inline(&self, path: &str, kind: &str, old_no: &str, new_no: &str) -> &[InlineThread] {
        let (side, no) = side_of(kind, old_no, new_no);
        self.review
            .as_ref()
            .and_then(|r| {
                r.threads
                    .get(&(path.to_owned(), side.to_owned(), no.to_owned()))
            })
            .map_or(&[], Vec::as_slice)
    }

    /// The link that opens a comment form under this line.
    #[must_use]
    pub fn comment_href(
        &self,
        path: &str,
        kind: &str,
        old_no: &str,
        new_no: &str,
    ) -> Option<String> {
        let r = self.review.as_ref().filter(|r| r.can_comment)?;
        let (side, no) = side_of(kind, old_no, new_no);
        if no.is_empty() || kind == "meta" {
            return None;
        }
        let sep = if r.back.contains('?') { '&' } else { '?' };
        Some(format!(
            "{}{sep}comment={side}:{no}:{}#composer",
            r.back,
            super::issues::enc(path)
        ))
    }

    /// Whether the comment form opens under this line.
    #[must_use]
    pub fn composing(&self, path: &str, kind: &str, old_no: &str, new_no: &str) -> bool {
        let (side, no) = side_of(kind, old_no, new_no);
        self.review
            .as_ref()
            .and_then(|r| r.composer.as_ref())
            .is_some_and(|(p, s, n)| p == path && s == side && n == no)
    }

    /// The URL of one file of this diff.
    #[must_use]
    pub fn file_href(&self, index: &usize) -> String {
        let sep = if self.href.contains('?') { '&' } else { '?' };
        let mut h = format!("{}{sep}file={index}", self.href);
        if !self.signed_in {
            h.push_str(&self.params());
        }
        h
    }

    /// The page in the unified view (for viewers who aren't signed in).
    #[must_use]
    pub fn unified_href(&self) -> String {
        self.with_options(false, self.ignore_whitespace)
    }

    /// The page in the split view.
    #[must_use]
    pub fn split_href(&self) -> String {
        self.with_options(true, self.ignore_whitespace)
    }

    /// The page with whitespace changes ignored or shown, the other way
    /// round.
    #[must_use]
    pub fn whitespace_href(&self) -> String {
        self.with_options(self.split, !self.ignore_whitespace)
    }

    fn with_options(&self, split: bool, ignore_whitespace: bool) -> String {
        let sep = if self.href.contains('?') { '&' } else { '?' };
        format!(
            "{}{sep}view={}&w={}",
            self.href,
            if split { "split" } else { "unified" },
            u8::from(ignore_whitespace)
        )
    }

    fn params(&self) -> String {
        format!(
            "&view={}&w={}",
            if self.split { "split" } else { "unified" },
            u8::from(self.ignore_whitespace)
        )
    }
}

/// The diff options in a page's query.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct DiffQuery {
    /// `unified` or `split`.
    pub view: Option<String>,
    /// `1` ignores whitespace.
    pub w: Option<String>,
    /// Show only this file (by position).
    pub file: Option<usize>,
    /// Open a comment form: `side:line:path`.
    pub comment: Option<String>,
}

impl DiffQuery {
    /// The line a comment form opens under: (path, side, line).
    #[must_use]
    pub fn composer(&self) -> Option<(String, String, String)> {
        let c = self.comment.as_deref()?;
        let mut parts = c.splitn(3, ':');
        let (side, line, path) = (parts.next()?, parts.next()?, parts.next()?);
        (matches!(side, "old" | "new") && line.parse::<u32>().is_ok() && !path.is_empty())
            .then(|| (path.to_owned(), side.to_owned(), line.to_owned()))
    }
}

impl DiffQuery {
    /// The options to use: the query's, else the viewer's preference.
    #[must_use]
    pub fn options(&self, ctx: &PageCtx) -> (bool, bool) {
        let split = self
            .view
            .as_deref()
            .map_or(ctx.diff_split, |v| v == "split");
        let ws = self
            .w
            .as_deref()
            .map_or(ctx.diff_ignore_whitespace, |v| v == "1");
        (split, ws)
    }

    #[must_use]
    pub fn git_options(&self, ctx: &PageCtx) -> slath_git::browse::DiffOptions {
        slath_git::browse::DiffOptions {
            ignore_whitespace: self.options(ctx).1,
        }
    }
}

fn status_name(s: Status) -> &'static str {
    match s {
        Status::Added => "Added",
        Status::Deleted => "Deleted",
        Status::Modified => "Modified",
        Status::Renamed => "Renamed",
        Status::Copied => "Copied",
    }
}

/// Pairs removed and added lines side by side: within a run of removals
/// followed by additions, row by row; context on both sides.
fn split_rows(lines: &[LineView]) -> Vec<SplitRow> {
    let cell = |l: &LineView, old: bool| Cell {
        kind: l.kind,
        no: if old {
            l.old_no.clone()
        } else {
            l.new_no.clone()
        },
        text: l.text.clone(),
        segments: l.segments.clone(),
    };
    let empty = || Cell {
        kind: "empty",
        no: String::new(),
        text: String::new(),
        segments: Vec::new(),
    };
    let mut rows = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        match lines[i].kind {
            "del" | "add" => {
                let dels: Vec<&LineView> =
                    lines[i..].iter().take_while(|l| l.kind == "del").collect();
                let adds: Vec<&LineView> = lines[i + dels.len()..]
                    .iter()
                    .take_while(|l| l.kind == "add")
                    .collect();
                for k in 0..dels.len().max(adds.len()) {
                    rows.push(SplitRow {
                        left: dels.get(k).map_or_else(empty, |l| cell(l, true)),
                        right: adds.get(k).map_or_else(empty, |l| cell(l, false)),
                    });
                }
                i += dels.len() + adds.len();
            }
            _ => {
                rows.push(SplitRow {
                    left: cell(&lines[i], true),
                    right: cell(&lines[i], false),
                });
                i += 1;
            }
        }
    }
    rows
}

/// Marks the changed characters of paired lines in each run of removals
/// followed by additions. A file with more pairs than the limit gets none.
fn refine(hunks: &mut [HunkView], bounds: &Bounds) {
    let refiner = CharacterRefiner;
    // (hunk, removed line, added line, marks)
    let mut found: Vec<(usize, usize, usize, slath_markup::intraline::Refined)> = Vec::new();
    for (hi, h) in hunks.iter().enumerate() {
        let mut i = 0;
        while i < h.lines.len() {
            if h.lines[i].kind != "del" {
                i += 1;
                continue;
            }
            let dels: Vec<usize> = (i..h.lines.len())
                .take_while(|k| h.lines[*k].kind == "del")
                .collect();
            let start = i + dels.len();
            let adds: Vec<usize> = (start..h.lines.len())
                .take_while(|k| h.lines[*k].kind == "add")
                .collect();
            if !adds.is_empty() {
                let old: Vec<&str> = dels.iter().map(|k| h.lines[*k].text.as_str()).collect();
                let new: Vec<&str> = adds.iter().map(|k| h.lines[*k].text.as_str()).collect();
                found.extend(
                    refiner
                        .refine(&old, &new, bounds)
                        .into_iter()
                        .map(|(a, b, r)| (hi, dels[a], adds[b], r)),
                );
            }
            i = start + adds.len();
        }
    }
    if found.len() > bounds.max_pairs {
        return;
    }
    for (hi, a, b, r) in found {
        hunks[hi].lines[a].segments = r.old;
        hunks[hi].lines[b].segments = r.new;
    }
}

/// The file tree: directories once, files below them.
fn tree(files: &[FileView]) -> Vec<TreeEntry> {
    let mut sorted: Vec<&FileView> = files.iter().collect();
    sorted.sort_by(|a, b| a.path.cmp(&b.path));
    let mut out: Vec<TreeEntry> = Vec::new();
    // The open directories, with their entries' indices.
    let mut open: Vec<(&str, usize)> = Vec::new();
    for f in sorted {
        let parts: Vec<&str> = f.path.split('/').collect();
        let (dirs, name) = parts.split_at(parts.len() - 1);
        let common = open
            .iter()
            .zip(dirs)
            .take_while(|((a, _), b)| a == *b)
            .count();
        open.truncate(common);
        for (depth, d) in dirs.iter().enumerate().skip(common) {
            open.push((d, out.len()));
            out.push(TreeEntry {
                depth,
                name: (*d).to_owned(),
                anchor: None,
                status: "",
                folded: true,
                reason: String::new(),
            });
        }
        if !f.folded {
            for (_, i) in &open {
                out[*i].folded = false;
            }
        }
        out.push(TreeEntry {
            depth: dirs.len(),
            name: name[0].to_owned(),
            anchor: Some(f.anchor.clone()),
            status: f.status,
            folded: f.folded,
            reason: if f.folded {
                f.reason.clone()
            } else {
                String::new()
            },
        });
    }
    out
}

/// Builds the view of a raw unified diff. `generated` are the paths marked
/// generated; `href` is the page URL without diff parameters.
#[must_use]
pub fn view(
    raw: &[u8],
    limits: &Limits,
    generated: &HashSet<Vec<u8>>,
    q: &DiffQuery,
    ctx: &PageCtx,
    href: String,
) -> DiffView {
    let diff = slath_git::diff::parse(raw, limits.diff_max_files, limits.diff_max_lines);
    let (split, ignore_whitespace) = q.options(ctx);
    let bounds = Bounds {
        max_line_bytes: limits.diff_intraline_max_line_bytes,
        max_pairs: limits.diff_intraline_max_pairs,
        ignore_whitespace,
    };
    // The inventory: every file in one class (as the API has it).
    let total = raw
        .split(|&b| b == b'\n')
        .filter(|l| l.starts_with(b"diff --git "))
        .count();
    let mut inventory =
        slath_git::inventory::inventory(&diff, total, limits.diff_max_files, generated);
    // `git diff -w` keeps the header of a file whose changes are all
    // whitespace; such files have nothing to show, and stay listed.
    let header_only = |f: &slath_git::diff::FileDiff| {
        ignore_whitespace && f.status == Status::Modified && !f.binary && f.hunks.is_empty()
    };
    for (e, f) in inventory.files.iter_mut().zip(&diff.files) {
        if header_only(f) && f.old_mode == f.new_mode {
            e.class = slath_git::inventory::FileClass::WhitespaceOnly;
            "only whitespace changed".clone_into(&mut e.reason);
        }
    }
    let mut files: Vec<FileView> = diff
        .files
        .iter()
        .zip(&inventory.files)
        .filter(|(f, _)| !header_only(f))
        .enumerate()
        .map(|(i, (f, e))| {
            let lines: usize = f.hunks.iter().map(|h| h.lines.len()).sum();
            let is_generated = generated.contains(f.path());
            let folded = e.class.folded();
            let load = if q.file == Some(i) {
                "now"
            } else if folded || lines > CLICK_LINES {
                "click"
            } else if i >= EAGER_FILES {
                "scroll"
            } else {
                "now"
            };
            let hunks = if load == "now" {
                f.hunks
                    .iter()
                    .map(|h| {
                        let lines: Vec<LineView> = h
                            .lines
                            .iter()
                            .map(|l| LineView {
                                kind: match l.kind {
                                    LineKind::Added => "add",
                                    LineKind::Removed => "del",
                                    LineKind::Context => "ctx",
                                    LineKind::NoNewline => "meta",
                                },
                                old_no: l.old_no.map(|n| n.to_string()).unwrap_or_default(),
                                new_no: l.new_no.map(|n| n.to_string()).unwrap_or_default(),
                                text: lossy(&l.text),
                                segments: Vec::new(),
                            })
                            .collect();
                        HunkView {
                            head: format!(
                                "@@ -{} +{} @@ {}",
                                h.old_start,
                                h.new_start,
                                lossy(&h.section)
                            ),
                            rows: Vec::new(),
                            lines,
                        }
                    })
                    .collect()
            } else {
                Vec::new()
            };
            let mut hunks = hunks;
            refine(&mut hunks, &bounds);
            if split {
                for h in &mut hunks {
                    h.rows = split_rows(&h.lines);
                }
            }
            FileView {
                index: i,
                anchor: format!("diff-{i}"),
                path: lossy(f.path()),
                old_path: matches!(f.status, Status::Renamed | Status::Copied)
                    .then(|| lossy(&f.old_path)),
                status: status_name(f.status),
                binary: f.binary,
                additions: f.additions,
                deletions: f.deletions,
                generated: is_generated,
                class: e.class.as_str(),
                reason: e.reason.clone(),
                folded,
                has_lines: lines > 0,
                viewed: false,
                load,
                hunks,
            }
        })
        .collect();
    let folded_files = files.iter().filter(|f| f.folded).count();
    let content_files = files.len() - folded_files;
    let tree = tree(&files);
    if let Some(i) = q.file {
        files.retain(|f| f.index == i);
    }
    DiffView {
        files,
        truncated: diff.truncated,
        split,
        ignore_whitespace,
        tree,
        href,
        only: q.file,
        signed_in: ctx.user.is_some(),
        review: None,
        content_files,
        folded_files,
        left_out: inventory.left_out,
        max_files: limits.diff_max_files,
    }
}

/// The paths of a raw diff (old and new), for the generated-file check.
#[must_use]
pub fn paths(raw: &[u8], limits: &Limits) -> Vec<Vec<u8>> {
    slath_git::diff::parse(raw, limits.diff_max_files, limits.diff_max_lines)
        .files
        .iter()
        .map(|f| f.path().to_vec())
        .collect()
}

/// The paths of `raw` marked generated in `source`'s `.gitattributes`.
/// Errors only cost the collapsing.
pub async fn generated(
    runner: &slath_git::Runner,
    git_dir: &std::path::Path,
    source: &slath_git::ObjectId,
    raw: &[u8],
    limits: &Limits,
) -> HashSet<Vec<u8>> {
    let paths = paths(raw, limits);
    slath_git::browse::generated_paths(runner, git_dir, source, &paths)
        .await
        .unwrap_or_else(|e| {
            tracing::warn!(error = %e, "reading generated-file attributes failed");
            HashSet::new()
        })
}

/// One file of a diff, for htmx requests that load it.
#[derive(Template)]
#[template(path = "repo/diff_file_fragment.html")]
pub struct FileFragment<'a> {
    pub ctx: &'a PageCtx,
    pub diff: &'a DiffView,
}

/// The full page, or for an htmx request that loads one file, just it.
pub fn respond(
    hx: super::HxRequest,
    ctx: &PageCtx,
    diff: &DiffView,
    full: &impl Template,
) -> Response {
    if hx.partial && diff.only.is_some() {
        super::render(&FileFragment { ctx, diff })
    } else {
        super::render(full)
    }
}

#[derive(Deserialize)]
struct PrefsForm {
    #[serde(default)]
    view: String,
    #[serde(default)]
    w: Option<String>,
    #[serde(default)]
    next: String,
}

/// `POST /-/settings/diff`: saves the diff options and returns to the page.
async fn save(
    State(state): State<AppState>,
    RequireSession(s): RequireSession,
    Form(f): Form<PrefsForm>,
) -> Result<Response, AppError> {
    state
        .services
        .set_diff_preferences(&s, &f.view, f.w.is_some())
        .await?;
    // Only local paths, never another site.
    let next = if f.next.starts_with('/') && !f.next.starts_with("//") {
        f.next
    } else {
        "/".to_owned()
    };
    Ok(Redirect::to(&next).into_response())
}

pub fn router() -> Router<AppState> {
    use axum::routing::post;
    Router::new().route_as("/-/settings/diff", Access::Account, post(save))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(kind: &'static str, text: &str) -> LineView {
        LineView {
            kind,
            old_no: String::new(),
            new_no: String::new(),
            text: text.to_owned(),
            segments: Vec::new(),
        }
    }

    fn hunk(lines: Vec<LineView>) -> HunkView {
        HunkView {
            head: String::new(),
            lines,
            rows: Vec::new(),
        }
    }

    #[test]
    fn paired_lines_get_marks_unless_the_file_has_too_many() {
        let bounds = Bounds {
            max_line_bytes: 1000,
            max_pairs: 1,
            ignore_whitespace: false,
        };
        let one = || hunk(vec![line("del", "let x = 1;"), line("add", "let x = 2;")]);
        let mut hunks = vec![one()];
        refine(&mut hunks, &bounds);
        let marked: Vec<&str> = hunks[0].lines[1]
            .segments
            .iter()
            .filter(|s| s.changed)
            .map(|s| s.text.as_str())
            .collect();
        assert_eq!(marked, vec!["2"]);
        // Two pairs in the file, one allowed: no marks anywhere.
        let mut hunks = vec![one(), one()];
        refine(&mut hunks, &bounds);
        assert!(
            hunks
                .iter()
                .all(|h| h.lines.iter().all(|l| l.segments.is_empty()))
        );
    }

    #[test]
    fn split_pairs_removals_with_additions() {
        let rows = split_rows(&[
            line("ctx", "a"),
            line("del", "b"),
            line("del", "c"),
            line("add", "B"),
            line("ctx", "d"),
            line("add", "e"),
        ]);
        let got: Vec<(&str, &str, &str, &str)> = rows
            .iter()
            .map(|r| {
                (
                    r.left.kind,
                    r.left.text.as_str(),
                    r.right.kind,
                    r.right.text.as_str(),
                )
            })
            .collect();
        assert_eq!(
            got,
            vec![
                ("ctx", "a", "ctx", "a"),
                ("del", "b", "add", "B"),
                ("del", "c", "empty", ""),
                ("ctx", "d", "ctx", "d"),
                ("empty", "", "add", "e"),
            ]
        );
    }

    #[test]
    fn tree_lists_directories_once() {
        let file = |i: usize, path: &str| FileView {
            index: i,
            anchor: format!("diff-{i}"),
            path: path.to_owned(),
            old_path: None,
            status: "Modified",
            binary: false,
            additions: 0,
            deletions: 0,
            generated: false,
            viewed: false,
            load: "now",
            hunks: Vec::new(),
            class: "content",
            reason: String::new(),
            folded: false,
            has_lines: true,
        };
        let t = tree(&[
            file(0, "src/b.rs"),
            file(1, "README"),
            file(2, "src/a/x.rs"),
            file(3, "src/a.rs"),
        ]);
        let got: Vec<(usize, &str, bool)> = t
            .iter()
            .map(|e| (e.depth, e.name.as_str(), e.anchor.is_some()))
            .collect();
        assert_eq!(
            got,
            vec![
                (0, "README", true),
                (0, "src", false),
                (1, "a.rs", true),
                (1, "a", false),
                (2, "x.rs", true),
                (1, "b.rs", true),
            ]
        );
        // Directories with only folded files are folded too.
        let folded = |i: usize, path: &str, folded: bool| FileView {
            folded,
            reason: "a binary file".to_owned(),
            ..file(i, path)
        };
        let t = tree(&[
            folded(0, "src/a/x.rs", false),
            folded(1, "src/b/y.rs", true),
            folded(2, "README", true),
        ]);
        let got: Vec<(&str, bool, &str)> = t
            .iter()
            .map(|e| (e.name.as_str(), e.folded, e.reason.as_str()))
            .collect();
        assert_eq!(
            got,
            vec![
                ("README", true, "a binary file"),
                ("src", false, ""),
                ("a", false, ""),
                ("x.rs", false, ""),
                ("b", true, ""),
                ("y.rs", true, "a binary file"),
            ]
        );
    }
}
