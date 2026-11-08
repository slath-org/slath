// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Diffs as every page shows them (commit, compare, merge request
//! changes): unified and split views, ignoring whitespace, a file tree,
//! collapsed generated files, and files that load as they come into view.

use std::collections::HashSet;

use askama::Template;
use axum::extract::State;
use axum::response::{IntoResponse, Redirect, Response};
use axum::{Form, Router};
use serde::Deserialize;
use slath_core::config::Limits;
use slath_git::diff::{LineKind, Status};

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
}

/// One side of a split row; `kind` is `empty` for a blank side.
pub struct Cell {
    pub kind: &'static str,
    pub no: String,
    pub text: String,
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
}

impl DiffView {
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
    };
    let empty = || Cell {
        kind: "empty",
        no: String::new(),
        text: String::new(),
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

/// The file tree: directories once, files below them.
fn tree(files: &[FileView]) -> Vec<TreeEntry> {
    let mut sorted: Vec<&FileView> = files.iter().collect();
    sorted.sort_by(|a, b| a.path.cmp(&b.path));
    let mut out = Vec::new();
    let mut open: Vec<&str> = Vec::new();
    for f in sorted {
        let parts: Vec<&str> = f.path.split('/').collect();
        let (dirs, name) = parts.split_at(parts.len() - 1);
        let common = open.iter().zip(dirs).take_while(|(a, b)| a == b).count();
        open.truncate(common);
        for (depth, d) in dirs.iter().enumerate().skip(common) {
            out.push(TreeEntry {
                depth,
                name: (*d).to_owned(),
                anchor: None,
                status: "",
            });
            open.push(d);
        }
        out.push(TreeEntry {
            depth: dirs.len(),
            name: name[0].to_owned(),
            anchor: Some(f.anchor.clone()),
            status: f.status,
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
    // `git diff -w` keeps the header of a file whose changes are all
    // whitespace; such files have nothing to show.
    let mut files: Vec<FileView> = diff
        .files
        .iter()
        .filter(|f| {
            !(ignore_whitespace && f.status == Status::Modified && !f.binary && f.hunks.is_empty())
        })
        .enumerate()
        .map(|(i, f)| {
            let lines: usize = f.hunks.iter().map(|h| h.lines.len()).sum();
            let is_generated = generated.contains(f.path());
            let load = if q.file == Some(i) {
                "now"
            } else if is_generated || lines > CLICK_LINES {
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
                            })
                            .collect();
                        HunkView {
                            head: format!(
                                "@@ -{} +{} @@ {}",
                                h.old_start,
                                h.new_start,
                                lossy(&h.section)
                            ),
                            rows: if split {
                                split_rows(&lines)
                            } else {
                                Vec::new()
                            },
                            lines,
                        }
                    })
                    .collect()
            } else {
                Vec::new()
            };
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
                load,
                hunks,
            }
        })
        .collect();
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
        }
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
            load: "now",
            hunks: Vec::new(),
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
    }
}
