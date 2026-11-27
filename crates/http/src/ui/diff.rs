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
    /// The line highlighted, with the emphasis on top; `None`: plain.
    pub html: Option<super::MarkupHtml>,
}

/// One side of a split row; `kind` is `empty` for a blank side.
pub struct Cell {
    pub kind: &'static str,
    pub no: String,
    pub text: String,
    pub segments: Vec<Segment>,
    pub html: Option<super::MarkupHtml>,
}

/// Highlighted lines of a file's two sides (each the whole file, so
/// constructs spanning lines outside the hunks come out right), or why
/// the file is shown plain.
#[derive(Debug, Clone)]
pub enum Sides {
    Lines { old: Vec<String>, new: Vec<String> },
    Plain(String),
}

/// Sides by the file's shown path.
pub type Highlights = HashMap<Vec<u8>, Sides>;

pub struct SplitRow {
    pub left: Cell,
    pub right: Cell,
}

/// A link that shows hidden unchanged lines (spec: hidden lines).
pub struct Expander {
    /// "12 unchanged lines", "20 more lines".
    pub label: String,
    /// Its accessible name, with the label in it: "Show 12 unchanged
    /// lines", "Show 20 more lines above this change".
    pub name: String,
    /// `unfold-vertical` for a whole gap, else the direction it opens in
    /// (`chevron-up`, `chevron-down`).
    pub icon: &'static str,
    /// The new-side ranges to show with it, `a-b,c-d`.
    pub ranges: String,
}

pub struct HunkView {
    pub head: String,
    pub lines: Vec<LineView>,
    pub rows: Vec<SplitRow>,
    /// Links for the hidden lines before it.
    pub expand: Vec<Expander>,
    /// Expanded unchanged lines right before and right after it.
    pub above: Vec<LineView>,
    pub below: Vec<LineView>,
    pub above_rows: Vec<SplitRow>,
    pub below_rows: Vec<SplitRow>,
    /// Its key (spec: viewed hunks).
    pub key: String,
    /// Merge request diffs: the viewer marked it viewed.
    pub viewed: bool,
    /// A change's diff whose brief points at the code: no item points at
    /// this hunk (claimed coverage).
    pub unbriefed: bool,
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
    /// The viewer marked it viewed (and it hasn't changed since), or
    /// every hunk of it.
    pub viewed: bool,
    /// The keys of all its hunks, also when they aren't loaded.
    pub hunk_keys: Vec<String>,
    /// Links for the hidden lines below its last hunk.
    pub expand_end: Vec<Expander>,
    /// `now`, `scroll` (loads when it comes into view), or `click`.
    pub load: &'static str,
    pub hunks: Vec<HunkView>,
    /// Why the lines aren't highlighted, if they aren't.
    pub plain_note: Option<String>,
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
    /// Merge request diffs: the viewer marked the file viewed.
    pub viewed: bool,
    /// Merge request diffs: the file's threads, and the unresolved ones.
    pub threads: usize,
    pub unresolved: usize,
}

/// A thread shown under its line.
pub struct InlineThread {
    pub id: String,
    pub resolved: bool,
    /// Who marked it addressed while it's open.
    pub addressed_by: Option<String>,
    /// Carried from this earlier revision.
    pub from: Option<i32>,
    /// Where a finding thread came from.
    pub finding: Option<String>,
    /// (author, rendered body, pending, a finding's plain-text message)
    pub comments: Vec<(super::Who, super::MarkupHtml, bool, Option<String>)>,
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
    /// Merge request diffs: the first file not marked viewed, and the
    /// first unresolved thread (anchors), for "Next unviewed file" and
    /// "Next unresolved thread".
    pub next_unviewed: Option<String>,
    pub next_unresolved: Option<String>,
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
    /// Fills the file tree's viewed marks and thread counts and the
    /// review navigation from the files' viewed state and `threads`.
    pub fn mark_review(&mut self, threads: &HashMap<(String, String, String), Vec<InlineThread>>) {
        let mut counts: HashMap<&str, (usize, usize)> = HashMap::new();
        for ((path, _, _), list) in threads {
            let c = counts.entry(path.as_str()).or_default();
            c.0 += list.len();
            c.1 += list.iter().filter(|t| !t.resolved).count();
        }
        for e in &mut self.tree {
            let Some(anchor) = &e.anchor else { continue };
            let Some(f) = self.files.iter().find(|f| &f.anchor == anchor) else {
                continue;
            };
            e.viewed = f.viewed;
            (e.threads, e.unresolved) = counts.get(f.path.as_str()).copied().unwrap_or_default();
        }
        self.next_unviewed = self
            .files
            .iter()
            .find(|f| !f.viewed && !f.folded)
            .map(|f| f.anchor.clone());
        // In file order, then line order within a file.
        self.next_unresolved = self.files.iter().find_map(|f| {
            let mut open: Vec<(u64, &str)> = threads
                .iter()
                .filter(|((p, _, _), _)| *p == f.path)
                .flat_map(|((_, _, line), list)| {
                    list.iter()
                        .filter(|t| !t.resolved)
                        .map(move |t| (line.parse().unwrap_or(0), t.id.as_str()))
                })
                .collect();
            open.sort_unstable();
            open.first().map(|(_, id)| format!("thread-{id}"))
        });
    }

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

    /// The URL of one file of this diff with the unchanged lines `ranges`
    /// shown, for htmx (without the fragment).
    #[must_use]
    pub fn expand_url(&self, index: &usize, ranges: &str) -> String {
        format!("{}&expand={ranges}", self.file_href(index))
    }

    /// The same, at the hunk the lines are next to.
    #[must_use]
    pub fn expand_href(&self, index: &usize, ranges: &str, hunk: &usize) -> String {
        format!("{}#diff-{index}-h{hunk}", self.expand_url(index, ranges))
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
    /// With `file`: unchanged lines to show, as new-side ranges `a-b,c-d`.
    pub expand: Option<String>,
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
        html: l.html.clone(),
    };
    let empty = || Cell {
        kind: "empty",
        no: String::new(),
        text: String::new(),
        segments: Vec::new(),
        html: None,
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

/// Most ranges an expansion link carries, the largest gap shown in one
/// step, and the lines one step shows of a larger gap.
const MAX_RANGES: usize = 20;
const WHOLE_GAP: u32 = 40;
const STEP: u32 = 20;

/// `a-b,c-d` as sorted, merged ranges within `1..=total`; at most
/// [`MAX_RANGES`] are read, the rest ignored.
fn parse_ranges(text: &str, total: u32) -> Vec<(u32, u32)> {
    let mut out: Vec<(u32, u32)> = text
        .split(',')
        .take(MAX_RANGES)
        .filter_map(|r| {
            let (a, b) = r.trim().split_once('-')?;
            let (a, b) = (a.parse::<u32>().ok()?, b.parse::<u32>().ok()?);
            (a >= 1 && a <= b && a <= total).then(|| (a, b.min(total)))
        })
        .collect();
    out.sort_unstable();
    let mut merged: Vec<(u32, u32)> = Vec::new();
    for (a, b) in out {
        match merged.last_mut() {
            Some(last) if a <= last.1.saturating_add(1) => last.1 = last.1.max(b),
            _ => merged.push((a, b)),
        }
    }
    merged
}

/// `ranges` with `add`, as the `expand` parameter.
fn with_range(ranges: &[(u32, u32)], add: (u32, u32)) -> String {
    let text: Vec<String> = ranges
        .iter()
        .chain(std::iter::once(&add))
        .map(|(a, b)| format!("{a}-{b}"))
        .collect();
    let all = text.join(",");
    parse_ranges(&all, u32::MAX)
        .iter()
        .map(|(a, b)| format!("{a}-{b}"))
        .collect::<Vec<_>>()
        .join(",")
}

/// A hunk's first and last line on the new side; a hunk without any sits
/// after `new_start`, as `(new_start + 1, new_start)`.
fn new_span(h: &slath_git::diff::Hunk) -> (u32, u32) {
    let mut nos = h.lines.iter().filter_map(|l| l.new_no);
    match nos.next() {
        Some(first) => (first, nos.next_back().unwrap_or(first)),
        None => (h.new_start + 1, h.new_start),
    }
}

/// Fills `hunks` (the views of `file`'s hunks) with the shown unchanged
/// lines around them and the links for the hidden ones, from `new` (the
/// new side, highlighted) and the `shown` ranges. Returns the links for
/// the lines below the last hunk, which end up in its `below`.
fn expand_gaps(
    file: &slath_git::diff::FileDiff,
    hunks: &mut [HunkView],
    new: &[String],
    shown: &[(u32, u32)],
) -> Vec<Expander> {
    let total = u32::try_from(new.len()).unwrap_or(u32::MAX);
    let is_shown = |n: u32| shown.iter().any(|(a, b)| (*a..=*b).contains(&n));
    // Old minus new line numbers, before each hunk and at the end.
    let mut offset: i64 = 0;
    let mut offsets = Vec::new();
    for h in &file.hunks {
        offsets.push(offset);
        for l in &h.lines {
            match l.kind {
                LineKind::Removed => offset += 1,
                LineKind::Added => offset -= 1,
                _ => {}
            }
        }
    }
    let line = |n: u32, off: i64| LineView {
        kind: "ctx",
        old_no: (i64::from(n) + off).to_string(),
        new_no: n.to_string(),
        text: String::new(),
        segments: Vec::new(),
        html: Some(super::MarkupHtml::from_markup(
            new.get(n as usize - 1).cloned().unwrap_or_default(),
        )),
    };
    // `where_` says where the lines are when a step shows only some of
    // them ("above this change"), and so the direction it opens in.
    let link = |count: u32, where_: &str, add: (u32, u32)| {
        let label = if count <= WHOLE_GAP {
            format!(
                "{count} unchanged line{}",
                if count == 1 { "" } else { "s" }
            )
        } else {
            format!("{STEP} more lines")
        };
        let (name, icon) = if count <= WHOLE_GAP {
            (format!("Show {label}"), "unfold-vertical")
        } else if where_.starts_with("above") {
            (format!("Show {label} {where_}"), "chevron-up")
        } else {
            (format!("Show {label} {where_}"), "chevron-down")
        };
        Expander {
            label,
            name,
            icon,
            ranges: with_range(shown, add),
        }
    };
    let mut prev_last = 0;
    for (i, h) in file.hunks.iter().enumerate() {
        let (first, last) = new_span(h);
        let (lo, hi) = (prev_last + 1, first.saturating_sub(1).min(total));
        if lo <= hi {
            // Shown lines from the top (after the hunk above) and from the
            // bottom (before this one); the gap above the first hunk only
            // opens upwards from it.
            let top = if i == 0 {
                0
            } else {
                (lo..=hi).take_while(|n| is_shown(*n)).count() as u32
            };
            let bottom = (lo..=hi).rev().take_while(|n| is_shown(*n)).count() as u32;
            let size = hi - lo + 1;
            if top + bottom >= size {
                hunks[i].above = (lo..=hi).map(|n| line(n, offsets[i])).collect();
            } else {
                if top > 0 {
                    hunks[i - 1].below = (lo..lo + top).map(|n| line(n, offsets[i])).collect();
                }
                hunks[i].above = (hi + 1 - bottom..=hi)
                    .map(|n| line(n, offsets[i]))
                    .collect();
                let (from, to) = (lo + top, hi - bottom);
                let hidden = to - from + 1;
                if hidden <= WHOLE_GAP {
                    hunks[i].expand.push(link(hidden, "", (from, to)));
                } else {
                    if i > 0 {
                        hunks[i].expand.push(link(
                            hidden,
                            "below the change above",
                            (from, from + STEP - 1),
                        ));
                    }
                    let name = if i == 0 { "above" } else { "above this change" };
                    hunks[i]
                        .expand
                        .push(link(hidden, name, (to + 1 - STEP, to)));
                }
            }
        }
        prev_last = last.max(prev_last);
    }
    // Below the last hunk: it opens downwards.
    let (lo, hi) = (prev_last + 1, total);
    let mut end = Vec::new();
    if lo <= hi {
        let top = (lo..=hi).take_while(|n| is_shown(*n)).count() as u32;
        if let Some(last) = hunks.last_mut() {
            last.below = (lo..lo + top).map(|n| line(n, offset)).collect();
        }
        if lo + top <= hi {
            let (from, to) = (lo + top, hi);
            let hidden = to - from + 1;
            end.push(if hidden <= WHOLE_GAP {
                link(hidden, "", (from, to))
            } else {
                link(hidden, "below", (from, from + STEP - 1))
            });
        }
    }
    end
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

/// Puts each line's highlighted HTML (from its side of the file) in
/// place, with the character emphasis on top. A file whose sides don't
/// match its lines stays plain.
fn apply_highlights(hunks: &mut [HunkView], sides: &Sides) -> Option<String> {
    let (old, new) = match sides {
        Sides::Lines { old, new } => (old, new),
        Sides::Plain(why) => return Some(why.clone()),
    };
    let pick = |l: &LineView| -> Option<&String> {
        let (side, no) = if l.kind == "del" {
            (old, &l.old_no)
        } else {
            (new, &l.new_no)
        };
        side.get(no.parse::<usize>().ok()?.checked_sub(1)?)
    };
    let fits = hunks
        .iter()
        .flat_map(|h| &h.lines)
        .filter(|l| l.kind != "meta")
        .all(|l| pick(l).is_some());
    if !fits {
        return Some("Shown without highlighting: the file's lines didn't match.".to_owned());
    }
    for l in hunks.iter_mut().flat_map(|h| h.lines.iter_mut()) {
        if l.kind == "meta" {
            continue;
        }
        let Some(base) = pick(l).cloned() else {
            continue;
        };
        let html = if l.segments.is_empty() {
            base
        } else {
            let mut ranges = Vec::new();
            let mut at = 0;
            for sg in &l.segments {
                let n = sg.text.chars().count();
                if sg.changed {
                    ranges.push((at, at + n));
                }
                at += n;
            }
            let tag = if l.kind == "del" { "del" } else { "ins" };
            slath_markup::emphasize(
                &base,
                &ranges,
                &format!("<{tag}><span class=\"visually-hidden\">changed: </span>"),
                &format!("<{tag}>"),
                &format!("</{tag}>"),
            )
        };
        l.html = Some(super::MarkupHtml::from_markup(html));
    }
    None
}

/// The shown path's highlighted sides for the files of `raw` that load
/// with the page (all of them, or only `q.file`), within the limits:
/// each side up to `diff_highlight_max_file_bytes`, all of them up to
/// `diff_highlight_max_bytes`, and two seconds of highlighting. Errors
/// only cost the highlighting.
pub async fn highlights(
    runner: &slath_git::Runner,
    git_dir: &std::path::Path,
    raw: &[u8],
    limits: &Limits,
    q: &DiffQuery,
    ctx: &PageCtx,
) -> Highlights {
    let mut out = Highlights::new();
    let diff = slath_git::diff::parse(raw, limits.diff_max_files, limits.diff_max_lines);
    let (_, ignore_whitespace) = q.options(ctx);
    let Ok(mut reader) = slath_git::ObjectReader::open(runner, git_dir).await else {
        return out;
    };
    let per_file = limits.diff_highlight_max_file_bytes.as_usize();
    let mut budget = limits.diff_highlight_max_bytes.as_usize();
    // (path, old path, old text, new path, new text)
    let mut sources: Vec<(Vec<u8>, String, String, String, String)> = Vec::new();
    let shown = diff.files.iter().filter(|f| {
        !(ignore_whitespace && f.status == Status::Modified && !f.binary && f.hunks.is_empty())
    });
    for (i, f) in shown.enumerate() {
        let lines: usize = f.hunks.iter().map(|h| h.lines.len()).sum();
        let wanted = match q.file {
            Some(only) => only == i,
            None => lines <= CLICK_LINES && i < EAGER_FILES,
        };
        if !wanted || f.binary || f.hunks.is_empty() {
            continue;
        }
        let mut texts = [String::new(), String::new()];
        let mut why = None;
        for (slot, blob) in [&f.old_blob, &f.new_blob].into_iter().enumerate() {
            let Some(oid) = blob
                .as_deref()
                .and_then(|b| slath_git::ObjectId::parse(b).ok())
            else {
                continue;
            };
            match reader.read_blob(&oid, per_file).await {
                Ok(slath_git::object::Blob::Data(d)) if d.len() <= budget => {
                    budget -= d.len();
                    texts[slot] = String::from_utf8_lossy(&d).into_owned();
                }
                Ok(slath_git::object::Blob::Data(_)) => {
                    why = Some(
                        "Shown without highlighting: this diff is too large to highlight."
                            .to_owned(),
                    );
                }
                Ok(slath_git::object::Blob::TooLarge { .. }) => {
                    why = Some(format!(
                        "Shown without highlighting: the file is larger than {} KiB.",
                        per_file / 1024
                    ));
                }
                Err(_) => why = Some("Shown without highlighting.".to_owned()),
            }
        }
        let [old, new] = texts;
        match why {
            Some(w) => {
                out.insert(f.path().to_vec(), Sides::Plain(w));
            }
            None => sources.push((
                f.path().to_vec(),
                lossy(&f.old_path),
                old,
                lossy(&f.new_path),
                new,
            )),
        }
    }
    if sources.is_empty() {
        return out;
    }
    let paths: Vec<Vec<u8>> = sources.iter().map(|s| s.0.clone()).collect();
    let work = tokio::task::spawn_blocking(move || {
        sources
            .into_iter()
            .map(|(path, old_path, old, new_path, new)| {
                let side = |p: &str, t: &str| {
                    if t.is_empty() {
                        Vec::new()
                    } else {
                        slath_markup::highlight(p, t).lines
                    }
                };
                (path, side(&old_path, &old), side(&new_path, &new))
            })
            .collect::<Vec<_>>()
    });
    match tokio::time::timeout(std::time::Duration::from_secs(2), work).await {
        Ok(Ok(done)) => {
            for (path, old, new) in done {
                out.insert(path, Sides::Lines { old, new });
            }
        }
        _ => {
            for p in paths {
                out.insert(
                    p,
                    Sides::Plain("Shown without highlighting: it took too long.".to_owned()),
                );
            }
        }
    }
    out
}

/// Highlights from a diff's own lines, for a sample diff without a
/// repository (the design catalog): each side holds the hunks' lines at
/// their numbers, so constructs outside the hunks aren't seen.
#[must_use]
pub fn sample_highlights(raw: &[u8], limits: &Limits) -> Highlights {
    let diff = slath_git::diff::parse(raw, limits.diff_max_files, limits.diff_max_lines);
    let mut out = Highlights::new();
    for f in &diff.files {
        // Lines the sample doesn't have (between its hunks) read as such
        // when they're expanded.
        let mut sides = [Vec::<Option<String>>::new(), Vec::<Option<String>>::new()];
        for l in f.hunks.iter().flat_map(|h| &h.lines) {
            for (slot, no, on_side) in [
                (0, l.old_no, l.kind != LineKind::Added),
                (1, l.new_no, l.kind != LineKind::Removed),
            ] {
                if let (true, Some(n)) = (on_side && l.kind != LineKind::NoNewline, no) {
                    let side = &mut sides[slot];
                    let n = n as usize;
                    if side.len() < n {
                        side.resize(n, None);
                    }
                    side[n - 1] = Some(lossy(&l.text));
                }
            }
        }
        let path = lossy(f.path());
        let sides = sides.map(|lines| {
            lines
                .into_iter()
                .map(|l| l.unwrap_or_else(|| "// …".to_owned()))
                .collect::<Vec<_>>()
        });
        let [old, new] = sides.map(|lines| slath_markup::highlight(&path, &lines.join("\n")).lines);
        out.insert(f.path().to_vec(), Sides::Lines { old, new });
    }
    out
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
                viewed: false,
                threads: 0,
                unresolved: 0,
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
            viewed: f.viewed,
            threads: 0,
            unresolved: 0,
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
    highlights: &Highlights,
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
            let keys = slath_git::diff::hunk_keys(f);
            let hunks = if load == "now" {
                f.hunks
                    .iter()
                    .zip(&keys)
                    .map(|(h, key)| {
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
                                html: None,
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
                            key: key.clone(),
                            viewed: false,
                            unbriefed: false,
                            expand: Vec::new(),
                            above: Vec::new(),
                            below: Vec::new(),
                            above_rows: Vec::new(),
                            below_rows: Vec::new(),
                        }
                    })
                    .collect()
            } else {
                Vec::new()
            };
            let mut hunks = hunks;
            refine(&mut hunks, &bounds);
            let plain_note = highlights
                .get(f.path())
                .and_then(|sides| apply_highlights(&mut hunks, sides));
            // Hidden lines expand where the new side was read (spec:
            // hidden lines); the ranges hold only for the file shown alone.
            let mut expand_end = Vec::new();
            if let (false, Some(Sides::Lines { new, .. })) = (
                hunks.is_empty() || matches!(f.status, Status::Added | Status::Deleted),
                highlights.get(f.path()),
            ) {
                let total = u32::try_from(new.len()).unwrap_or(u32::MAX);
                let shown = match (&q.expand, q.file) {
                    (Some(e), Some(only)) if only == i => parse_ranges(e, total),
                    _ => Vec::new(),
                };
                expand_end = expand_gaps(f, &mut hunks, new, &shown);
            }
            if split {
                for h in &mut hunks {
                    h.rows = split_rows(&h.lines);
                    h.above_rows = split_rows(&h.above);
                    h.below_rows = split_rows(&h.below);
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
                hunk_keys: keys,
                load,
                hunks,
                expand_end,
                plain_note,
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
        next_unviewed: None,
        next_unresolved: None,
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

/// The full page, or for an htmx request that loads one file, just it,
/// with `Vary: HX-Request` so caches keep the two apart.
pub fn respond(
    hx: super::HxRequest,
    ctx: &PageCtx,
    diff: &DiffView,
    full: &impl Template,
) -> Response {
    let mut res = if hx.partial && diff.only.is_some() {
        super::render(&FileFragment { ctx, diff })
    } else {
        super::render(full)
    };
    res.headers_mut().insert(
        axum::http::header::VARY,
        axum::http::HeaderValue::from_static("HX-Request"),
    );
    res
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

    #[test]
    fn conf_review_17_expand_ranges_are_sorted_merged_and_bounded() {
        assert_eq!(parse_ranges("9-16,1-1,15-20", 40), vec![(1, 1), (9, 20)]);
        assert_eq!(parse_ranges("30-99,0-3,5-2,x-1,41-45", 40), vec![(30, 40)]);
        let many: Vec<String> = (0..30).map(|i| format!("{0}-{0}", i * 2 + 1)).collect();
        assert_eq!(parse_ranges(&many.join(","), 1000).len(), 20);
        assert_eq!(with_range(&[(9, 12)], (13, 16)), "9-16");
    }

    /// A 100-line file with changes at lines 5 and 60 (3 lines of context).
    fn gappy() -> (slath_git::diff::FileDiff, Vec<String>) {
        let raw = "diff --git a/f b/f\nindex 1111111..2222222 100644\n--- a/f\n+++ b/f\n\
                   @@ -2,7 +2,7 @@\n 2\n 3\n 4\n-5\n+five\n 6\n 7\n 8\n\
                   @@ -57,7 +57,8 @@\n 57\n 58\n 59\n-60\n+sixty\n+sixty-one\n 61\n 62\n 63\n";
        let d = slath_git::diff::parse(raw.as_bytes(), 10, 1000);
        let new = (1..=101).map(|n| format!("<span>{n}</span>")).collect();
        (d.files[0].clone(), new)
    }

    fn views(f: &slath_git::diff::FileDiff) -> Vec<HunkView> {
        f.hunks.iter().map(|_| hunk(Vec::new())).collect()
    }

    #[test]
    fn conf_review_17_gaps_offer_their_lines() {
        let (f, new) = gappy();
        let mut h = views(&f);
        let end = expand_gaps(&f, &mut h, &new, &[]);
        // Above the first hunk: line 1 alone.
        let labels = |e: &[Expander]| {
            e.iter()
                .map(|x| (x.label.clone(), x.ranges.clone()))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            labels(&h[0].expand),
            vec![("1 unchanged line".into(), "1-1".into())]
        );
        // 9..=56: 48 lines, 20 at each end.
        assert_eq!(
            labels(&h[1].expand),
            vec![
                ("20 more lines".into(), "9-28".into()),
                ("20 more lines".into(), "37-56".into())
            ]
        );
        assert_eq!(
            h[1].expand[0].name,
            "Show 20 more lines below the change above"
        );
        assert_eq!(
            (h[1].expand[0].icon, h[1].expand[1].icon),
            ("chevron-down", "chevron-up")
        );
        assert_eq!(h[1].expand[1].name, "Show 20 more lines above this change");
        // Below the last hunk: 65..=101 (37 lines, the new side has one more).
        assert_eq!(
            labels(&end),
            vec![("37 unchanged lines".into(), "65-101".into())]
        );
        assert!(h.iter().all(|x| x.above.is_empty() && x.below.is_empty()));
    }

    #[test]
    fn conf_review_17_shown_lines_sit_next_to_their_hunks() {
        let (f, new) = gappy();
        let mut h = views(&f);
        let end = expand_gaps(&f, &mut h, &new, &[(1, 1), (9, 28), (37, 56), (65, 70)]);
        assert!(h[0].expand.is_empty());
        assert_eq!(h[0].above.len(), 1);
        // Both ends of the large gap are shown next to their hunks; the 8
        // lines between them have one link. No lines moved before them, so
        // both sides have the same numbers.
        let nos = |v: &[LineView]| -> Vec<(String, String)> {
            v.iter()
                .map(|l| (l.old_no.clone(), l.new_no.clone()))
                .collect()
        };
        assert_eq!(nos(&h[0].below).first(), Some(&("9".into(), "9".into())));
        assert_eq!(h[0].below.len(), 20);
        assert_eq!(nos(&h[1].above).last(), Some(&("56".into(), "56".into())));
        assert_eq!(h[1].above.len(), 20);
        assert_eq!(h[1].expand.len(), 1);
        assert_eq!(h[1].expand[0].label, "8 unchanged lines");
        assert_eq!(h[1].expand[0].name, "Show 8 unchanged lines");
        assert_eq!(h[1].expand[0].icon, "unfold-vertical");
        assert_eq!(h[1].expand[0].ranges, "1-1,9-56,65-70");
        // Below the last hunk: shown lines, then the rest; old numbers are
        // one behind (a line was added).
        assert_eq!(
            h[1].below
                .first()
                .map(|l| (l.old_no.as_str(), l.new_no.as_str())),
            Some(("64", "65"))
        );
        assert_eq!(h[1].below.len(), 6);
        assert_eq!(end[0].label, "31 unchanged lines");
        assert_eq!(end[0].ranges, "1-1,9-28,37-56,65-101");
    }

    use super::*;

    fn line(kind: &'static str, text: &str) -> LineView {
        LineView {
            kind,
            old_no: String::new(),
            new_no: String::new(),
            text: text.to_owned(),
            segments: Vec::new(),
            html: None,
        }
    }

    fn hunk(lines: Vec<LineView>) -> HunkView {
        HunkView {
            head: String::new(),
            lines,
            rows: Vec::new(),
            key: String::new(),
            viewed: false,
            unbriefed: false,
            expand: Vec::new(),
            above: Vec::new(),
            below: Vec::new(),
            above_rows: Vec::new(),
            below_rows: Vec::new(),
        }
    }

    fn numbered(kind: &'static str, old: &str, new: &str, text: &str) -> LineView {
        LineView {
            old_no: old.to_owned(),
            new_no: new.to_owned(),
            ..line(kind, text)
        }
    }

    /// The text of HTML, without tags and with entities decoded.
    fn text_of(html: &str) -> String {
        let mut out = String::new();
        let mut tag = false;
        for c in html.chars() {
            match c {
                '<' => tag = true,
                '>' => tag = false,
                c if !tag => out.push(c),
                _ => {}
            }
        }
        out.replace("&lt;", "<")
            .replace("&gt;", ">")
            .replace("&quot;", "\"")
            .replace("&#39;", "'")
            .replace("&amp;", "&")
    }

    #[test]
    fn highlighted_lines_keep_their_text_and_emphasis() {
        let old = "/* a comment\n   spanning lines */\nlet x = 1;\n";
        let new = "/* a comment\n   spanning lines */\nlet x = 2;\n";
        let lines = || {
            vec![
                numbered("ctx", "2", "2", "   spanning lines */"),
                numbered("del", "3", "", "let x = 1;"),
                numbered("add", "", "3", "let x = 2;"),
            ]
        };
        let bounds = Bounds {
            max_line_bytes: 1000,
            max_pairs: 10,
            ignore_whitespace: false,
        };
        let mut plain = vec![hunk(lines())];
        refine(&mut plain, &bounds);
        let mut lit = vec![hunk(lines())];
        refine(&mut lit, &bounds);
        let sides = Sides::Lines {
            old: slath_markup::highlight("x.rs", old).lines,
            new: slath_markup::highlight("x.rs", new).lines,
        };
        assert_eq!(apply_highlights(&mut lit, &sides), None);
        for (p, l) in plain[0].lines.iter().zip(&lit[0].lines) {
            let html = l.html.as_ref().unwrap().to_string();
            // The same text and the same rows.
            let shown = text_of(&html).replace("changed: ", "");
            assert_eq!(shown, p.text, "{html}");
            // The same emphasis.
            let marks = p.segments.iter().filter(|s| s.changed).count();
            let tag = if l.kind == "del" { "<del>" } else { "<ins>" };
            assert_eq!(html.matches(tag).count(), marks, "{html}");
        }
        // The comment's second line is a comment, though its start is
        // outside the hunk.
        let ctx = lit[0].lines[0].html.as_ref().unwrap().to_string();
        assert!(ctx.contains("syn-comment"), "{ctx}");
        // Sides that don't fit the line numbers leave the file plain.
        let short = Sides::Lines {
            old: Vec::new(),
            new: Vec::new(),
        };
        let mut again = vec![hunk(lines())];
        assert!(apply_highlights(&mut again, &short).is_some());
        assert!(again[0].lines.iter().all(|l| l.html.is_none()));
        let mut noted = vec![hunk(lines())];
        let why = Sides::Plain("too large".to_owned());
        assert_eq!(
            apply_highlights(&mut noted, &why).as_deref(),
            Some("too large")
        );
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
            hunk_keys: Vec::new(),
            expand_end: Vec::new(),
            load: "now",
            hunks: Vec::new(),
            plain_note: None,
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
