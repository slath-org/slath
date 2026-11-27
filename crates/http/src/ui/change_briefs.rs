// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Change briefs in the UI: the Brief section of a change's page (the
//! shown version's newest edit), the comparison of two versions' briefs,
//! and the page that writes one
//! (`…/merge-requests/{n}/changes/{change}/brief`). A brief is its
//! author's claim, shown as plain text: a list of items, grouped by kind.

use askama::Template;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Redirect, Response};
use axum::{Form, Router};
use slath_db::merge_requests::MergeRequest;
use slath_service::brief_schema::{Anchor, Compared, Item, Kind, SCHEMA, Side, compare, numbered};
use slath_service::change_briefs::ChangeBrief;
use slath_service::{FieldError, ServiceError};

use super::repos::RepoHeader;
use super::settings::error_for;
use super::{PageCtx, Time, Who, render};
use crate::AppState;
use crate::auth::RequireSession;
use crate::error::AppError;
use crate::routes::{Access, RouteExt};

/// A version's brief as the change's page shows it.
pub struct BriefView {
    pub id: String,
    pub author: Who,
    pub created: Time,
    /// How many edits the version's brief has (1 without edits).
    pub edits: usize,
    /// Its items, by kind in the order a brief shows them.
    pub items: Vec<ItemView>,
    /// When an item points at the code: how many changed hunks they point
    /// at, of how many, and the others.
    pub coverage: Option<CoverageView>,
}

/// Claimed coverage as the brief shows it.
pub struct CoverageView {
    pub covered: usize,
    pub hunks: usize,
    /// The first hunks no item points at ("big.txt, lines 1–30"), with
    /// links into the diff once found, and how many more there are.
    pub uncovered: Vec<(String, Option<String>)>,
    pub more: usize,
    places: Vec<slath_service::brief_schema::HunkPlace>,
}

/// The most uncovered hunks the brief lists by name.
const UNCOVERED_LISTED: usize = 10;

impl CoverageView {
    fn new(c: &slath_service::brief_schema::Coverage) -> Self {
        let name = |h: &slath_service::brief_schema::HunkPlace| {
            let (start, n, removed) = if h.new_lines > 0 {
                (h.new_start, h.new_lines, "")
            } else {
                (h.old_start, h.old_lines, " (removed)")
            };
            if n <= 1 {
                format!("{}, line {start}{removed}", h.path)
            } else {
                format!("{}, lines {start}–{}{removed}", h.path, start + n - 1)
            }
        };
        Self {
            covered: c.covered,
            hunks: c.hunks,
            uncovered: c
                .uncovered
                .iter()
                .take(UNCOVERED_LISTED)
                .map(|h| (name(h), None))
                .collect(),
            more: c.uncovered.len().saturating_sub(UNCOVERED_LISTED),
            places: c.uncovered.clone(),
        }
    }
}

/// One item of the shown brief, with the threads on it.
pub struct ItemView {
    /// Its index in the edit's items, and its name ("Effect 2").
    pub index: usize,
    pub name: String,
    pub text: String,
    pub before: Option<String>,
    pub after: Option<String>,
    /// Where it points, as links into the diff below once they're found.
    pub places: Vec<Place>,
    /// Each thread's ID, starter, number of comments, and whether it's
    /// resolved.
    pub threads: Vec<(String, Who, usize, bool)>,
    /// Opens the comment form under the item, for those who may comment.
    pub comment_href: Option<String>,
    /// The comment form is open here.
    pub form_open: bool,
    /// Under the last verification item: the checks reported on the
    /// version's own commit (context, state, reporter), since what it
    /// claims can be held against them, and never statuses of another
    /// commit.
    pub checks: Option<Vec<(String, String, Who)>>,
}

/// A place an item points at.
pub struct Place {
    /// As written: `src/a.rs:10–20`.
    pub text: String,
    anchor: Anchor,
    /// The hunk (or the file) in the page's diff.
    pub href: Option<String>,
}

impl Place {
    fn new(a: &Anchor) -> Self {
        Self {
            text: a
                .notation()
                .replacen('-', "–", usize::from(a.lines().is_some())),
            anchor: a.clone(),
            href: None,
        }
    }
}

/// Who views the change's page, as the brief section needs it.
pub struct Viewer<'a> {
    pub subject: &'a slath_core::authz::Subject,
    /// May write briefs (the latest version's) and comment.
    pub write: bool,
    pub comment: bool,
    /// The item whose comment form is open (`?brief_comment=`).
    pub open: Option<usize>,
}

/// One item of two versions' briefs compared: its name, `new`,
/// `updated`, or `removed`, and its lines compared.
pub struct ItemDiff {
    pub name: String,
    pub mark: &'static str,
    pub diff: Result<Vec<super::merge_requests::DescriptionLine>, ()>,
}

/// An earlier version's items shown on a version without a brief: those
/// whose lines are unchanged here, at their places here.
pub struct CarriedView {
    pub from: usize,
    /// The earlier version's brief.
    pub href: String,
    pub items: Vec<ItemView>,
    /// How many of its items stay there.
    pub left: usize,
}

/// What the change's page shows of briefs.
pub struct BriefParts {
    /// The shown version's brief, when not comparing.
    pub shown: Option<BriefView>,
    /// Without one: what carries forward from an earlier version's.
    pub carried: Option<CarriedView>,
    /// The items that differ when comparing two versions, and how many
    /// are the same.
    pub changed: Vec<ItemDiff>,
    pub unchanged: usize,
    /// The page that writes the brief of the latest version, for those
    /// who may, with whether one exists.
    pub write: Option<(String, bool)>,
    /// Where an item's comment form posts, and the page it returns to.
    pub threads_action: String,
    pub back: String,
}

impl BriefParts {
    /// Links each place of the shown brief to its hunk in `diff` (or to
    /// its file, when the hunk isn't loaded).
    pub fn link(&mut self, diff: &mut super::diff::DiffView) {
        if let Some(c) = &mut self.carried {
            link_places(&mut c.items, diff);
        }
        let Some(b) = &mut self.shown else { return };
        // Hunks no item points at say so in their header, and the brief
        // links to them.
        if let Some(c) = &mut b.coverage {
            for (i, h) in c.places.iter().enumerate() {
                let head = format!("@@ -{} +{} @@", h.old_start, h.new_start);
                let Some(f) = diff.files.iter_mut().find(|f| f.path == h.path) else {
                    continue;
                };
                let found = f.hunks.iter().position(|x| x.head.starts_with(&head));
                if let Some(k) = found {
                    f.hunks[k].unbriefed = true;
                }
                if let Some(entry) = c.uncovered.get_mut(i) {
                    entry.1 = Some(match found {
                        Some(k) => format!("#{}-h{k}", f.anchor),
                        None => format!("#{}", f.anchor),
                    });
                }
            }
        }
        link_places(&mut b.items, diff);
    }

    /// "1 new, 1 updated, 3 unchanged" for a comparison.
    #[must_use]
    pub fn summary(&self) -> String {
        let count = |mark: &str| self.changed.iter().filter(|d| d.mark == mark).count();
        [
            (count("new"), "new"),
            (count("updated"), "updated"),
            (count("removed"), "removed"),
            (self.unchanged, "unchanged"),
        ]
        .into_iter()
        .filter(|(n, _)| *n > 0)
        .map(|(n, what)| format!("{n} {what}"))
        .collect::<Vec<_>>()
        .join(", ")
    }
}

/// Links each place of `items` to its hunk in `diff` (or to its file,
/// when the hunk isn't loaded).
fn link_places(items: &mut [ItemView], diff: &super::diff::DiffView) {
    for p in items.iter_mut().flat_map(|i| i.places.iter_mut()) {
        let Some(f) = diff.files.iter().find(|f| f.path == p.anchor.path) else {
            continue;
        };
        let hunk = p.anchor.lines().and_then(|(start, _)| {
            let start = start.to_string();
            f.hunks.iter().position(|h| {
                h.lines.iter().any(|l| match p.anchor.side() {
                    Side::New => l.kind != "del" && l.new_no == start,
                    Side::Old => l.kind != "add" && l.old_no == start,
                })
            })
        });
        p.href = Some(match hunk {
            Some(k) => format!("#{}-h{k}", f.anchor),
            None => format!("#{}", f.anchor),
        });
    }
}

/// Indexes of `items` in the order a brief shows them: by kind, then as
/// the author ordered them.
fn shown_order(items: &[Item]) -> Vec<usize> {
    Kind::ALL
        .iter()
        .flat_map(|k| (0..items.len()).filter(move |i| items[*i].kind == *k))
        .collect()
}

/// The briefs of `change` for its page: `shown` is the version shown,
/// `from` the one compared from, `latest` the change's latest version's
/// commit; `writable` says whether the viewer may write briefs.
pub async fn parts(
    state: &AppState,
    m: &MergeRequest,
    (ns, repo, change): (&str, &str, &str),
    (shown, from, latest): (&str, Option<&str>, Option<&str>),
    viewer: &Viewer<'_>,
) -> Result<BriefParts, AppError> {
    let writable = viewer.write;
    let all = state
        .services
        .mr_change_briefs(m, change, None, 1000)
        .await?;
    let of = |commit: &str| all.iter().find(|b| b.brief.commit_oid == commit);
    let edits = |commit: &str| all.iter().filter(|b| b.brief.commit_oid == commit).count();
    let write = (writable && latest == Some(shown)).then(|| {
        (
            format!(
                "/{ns}/{repo}/merge-requests/{}/changes/{change}/brief",
                m.number
            ),
            of(shown).is_some(),
        )
    });
    let Some(from) = from else {
        let page = format!("/{ns}/{repo}/merge-requests/{}/changes/{change}", m.number);
        let carried = match of(shown) {
            Some(_) => None,
            None => state
                .services
                .carried_brief(m, change, shown)
                .await?
                .map(|c| carried_view(&c, &page)),
        };
        let shown = match of(shown) {
            None => None,
            Some(v) => {
                let threads = state
                    .services
                    .brief_threads(viewer.subject, m, v.brief.id)
                    .await?;
                let mut b = view(v, edits(shown), &threads, &page, viewer);
                if let Some(it) = b
                    .items
                    .iter_mut()
                    .rev()
                    .find(|it| v.items[it.index].kind == Kind::Verification)
                {
                    let mut statuses = state
                        .services
                        .commit_statuses(viewer.subject, (ns, repo), shown)
                        .await?;
                    statuses.sort_by(|a, b| a.context.cmp(&b.context));
                    it.checks = Some(
                        statuses
                            .into_iter()
                            .map(|st| (st.context, st.state, Who::account(st.reporter)))
                            .collect(),
                    );
                }
                Some(b)
            }
        };
        return Ok(BriefParts {
            shown,
            carried,
            changed: Vec::new(),
            unchanged: 0,
            write,
            threads_action: format!("/{ns}/{repo}/merge-requests/{}/threads", m.number),
            back: page,
        });
    };
    let old = of(from).map(|v| v.items.clone()).unwrap_or_default();
    let new = of(shown).map(|v| v.items.clone()).unwrap_or_default();
    let (old_names, new_names) = (numbered(&old), numbered(&new));
    let lines = |it: &Item| it.lines().join("\n");
    let mut changed = Vec::new();
    let mut unchanged = 0;
    for c in compare(&old, &new) {
        let (name, mark, a, b) = match c {
            Compared::Same(..) => {
                unchanged += 1;
                continue;
            }
            Compared::Updated(i, j) => (&new_names[j].1, "updated", lines(&old[i]), lines(&new[j])),
            Compared::New(j) => (&new_names[j].1, "new", String::new(), lines(&new[j])),
            Compared::Removed(i) => (&old_names[i].1, "removed", lines(&old[i]), String::new()),
        };
        if let Some(diff) = super::merge_requests::description_diff(&a, &b) {
            changed.push(ItemDiff {
                name: name.clone(),
                mark,
                diff,
            });
        }
    }
    Ok(BriefParts {
        shown: None,
        carried: None,
        changed,
        unchanged,
        write: None,
        threads_action: String::new(),
        back: String::new(),
    })
}

fn view(
    v: &ChangeBrief,
    edits: usize,
    threads: &[slath_service::mr_threads::BriefThread],
    page: &str,
    viewer: &Viewer<'_>,
) -> BriefView {
    BriefView {
        id: v.brief.id.to_string(),
        author: Who::account(v.author.clone()),
        created: v.brief.created_at.into(),
        edits,
        coverage: v.coverage.as_ref().map(CoverageView::new),
        items: item_views(&v.items, |_| true, threads, page, viewer),
    }
}

/// The items of `items` that `keep` keeps, in the order a brief shows
/// them, with their threads and, for `viewer`, links to comment.
fn item_views(
    items: &[Item],
    keep: impl Fn(usize) -> bool,
    threads: &[slath_service::mr_threads::BriefThread],
    page: &str,
    viewer: &Viewer<'_>,
) -> Vec<ItemView> {
    let names = numbered(items);
    shown_order(items)
        .into_iter()
        .filter(|i| keep(*i))
        .map(|i| {
            let it = &items[i];
            let reveal = |s: &str| slath_markup::reveal_bidi(s).into_owned();
            ItemView {
                index: i,
                name: names[i].1.clone(),
                text: reveal(&it.text),
                before: it.before.as_deref().map(reveal),
                after: it.after.as_deref().map(reveal),
                places: it.anchors.iter().map(Place::new).collect(),
                threads: threads
                    .iter()
                    .filter(|t| t.item == i)
                    .map(|t| {
                        (
                            t.id.to_string(),
                            Who::account(t.author.clone()),
                            t.comments,
                            t.resolved,
                        )
                    })
                    .collect(),
                comment_href: viewer
                    .comment
                    .then(|| format!("{page}?brief_comment={i}#brief-item-{i}")),
                form_open: viewer.comment && viewer.open == Some(i),
                checks: None,
            }
        })
        .collect()
}

/// What carries forward from an earlier version's brief, without threads
/// or comments: they belong to the brief they were written on.
fn carried_view(c: &slath_service::change_briefs::Carried, page: &str) -> CarriedView {
    let nobody = slath_core::authz::Subject::Anonymous;
    let viewer = Viewer {
        subject: &nobody,
        write: false,
        comment: false,
        open: None,
    };
    CarriedView {
        from: c.from_version,
        href: format!("{page}?rev={}#brief", c.from_revision),
        items: item_views(&c.items, |i| c.carries[i], &[], page, &viewer),
        left: c.carries.iter().filter(|x| !**x).count(),
    }
}

/// One kind's items in the form.
struct GroupView {
    kind: &'static str,
    heading: &'static str,
    hint: &'static str,
    /// "Add an effect".
    add: String,
    items: Vec<SlotView>,
}

/// One item in the form: its fields' ID prefix (`item-effect-0`), its
/// name, its text, and an effect's Before and After fields (ID, label,
/// value).
struct SlotView {
    id: String,
    name: String,
    text: String,
    /// Where it points, one place per line.
    places: String,
    extra: Vec<(String, &'static str, String)>,
}

#[derive(Template)]
#[template(path = "repo/change_brief.html")]
struct FormPage {
    ctx: PageCtx,
    header: RepoHeader,
    heading: String,
    action: String,
    back: String,
    commit: String,
    groups: Vec<GroupView>,
    /// The field to focus (the item just added).
    focus: Option<String>,
    errors: Vec<FieldError>,
}

impl FormPage {
    fn error(&self, field: &str) -> Option<&str> {
        error_for(&self.errors, field)
    }

    fn is_effect(kind: &str) -> bool {
        kind == Kind::Effect.as_str()
    }
}

/// The form's items: for each kind, its items in order (text, before,
/// after, and the places it points at, one per line).
#[derive(Default, Clone)]
struct Slots(Vec<(Kind, Vec<[String; 4]>)>);

impl Slots {
    fn of_items(items: &[Item]) -> Self {
        Self(
            Kind::ALL
                .iter()
                .map(|k| {
                    let mine = items
                        .iter()
                        .filter(|it| it.kind == *k)
                        .map(|it| {
                            [
                                it.text.clone(),
                                it.before.clone().unwrap_or_default(),
                                it.after.clone().unwrap_or_default(),
                                it.anchors
                                    .iter()
                                    .map(Anchor::notation)
                                    .collect::<Vec<_>>()
                                    .join("\n"),
                            ]
                        })
                        .collect();
                    (*k, mine)
                })
                .collect(),
        )
    }

    /// Reads `item-<kind>-<n>-<text|before|after|anchors>` fields.
    fn of_form(fields: &[(String, String)]) -> Self {
        let mut out = Self::of_items(&[]);
        for (key, value) in fields {
            let Some(rest) = key.strip_prefix("item-") else {
                continue;
            };
            let mut parts = rest.splitn(3, '-');
            let (Some(kind), Some(n), Some(part)) = (parts.next(), parts.next(), parts.next())
            else {
                continue;
            };
            let (Some(kind), Ok(n)) = (Kind::parse(kind), n.parse::<usize>()) else {
                continue;
            };
            let at = match part {
                "text" => 0,
                "before" => 1,
                "after" => 2,
                "anchors" => 3,
                _ => continue,
            };
            // A form names at most a few dozen items per kind.
            if n > 200 {
                continue;
            }
            if let Some((_, slots)) = out.0.iter_mut().find(|(k, _)| *k == kind) {
                if slots.len() <= n {
                    slots.resize(n + 1, Default::default());
                }
                slots[n][at].clone_from(value);
            }
        }
        out
    }

    /// The items to save, and each one's field ID prefix; items with
    /// nothing typed are dropped.
    fn items(&self) -> (Vec<serde_json::Value>, Vec<String>) {
        let mut items = Vec::new();
        let mut ids = Vec::new();
        for (kind, slots) in &self.0 {
            for (n, slot) in slots.iter().enumerate() {
                if slot.iter().all(|v| v.trim().is_empty()) {
                    continue;
                }
                let [text, before, after, places] = slot;
                let mut v = serde_json::json!({"kind": kind.as_str(), "text": text});
                if *kind == Kind::Effect {
                    v["before"] = before.clone().into();
                    v["after"] = after.clone().into();
                }
                let anchors: Vec<Anchor> = places.lines().filter_map(Anchor::parse).collect();
                if !anchors.is_empty() {
                    v["anchors"] = serde_json::to_value(anchors).unwrap_or_default();
                }
                items.push(v);
                ids.push(format!("item-{}-{n}", kind.as_str()));
            }
        }
        (items, ids)
    }

    fn groups(&self) -> Vec<GroupView> {
        self.0
            .iter()
            .map(|(kind, slots)| {
                let label = kind.label();
                let article = if label.starts_with(['A', 'E', 'I', 'O', 'U']) {
                    "an"
                } else {
                    "a"
                };
                GroupView {
                    kind: kind.as_str(),
                    heading: kind.heading(),
                    hint: kind.hint(),
                    add: format!("Add {article} {}", label.to_lowercase()),
                    items: slots
                        .iter()
                        .enumerate()
                        .map(|(n, [text, before, after, places])| {
                            let id = format!("item-{}-{n}", kind.as_str());
                            SlotView {
                                name: if slots.len() > 1 {
                                    format!("{label} {}", n + 1)
                                } else {
                                    label.to_owned()
                                },
                                text: text.clone(),
                                places: places.clone(),
                                extra: if *kind == Kind::Effect {
                                    vec![
                                        (format!("{id}-before"), "Before", before.clone()),
                                        (format!("{id}-after"), "After", after.clone()),
                                    ]
                                } else {
                                    Vec::new()
                                },
                                id,
                            }
                        })
                        .collect(),
                }
            })
            .collect()
    }
}

/// The form page's frame for merge request `n`'s change `change`, with
/// `values` typed so far (and errors), or the version's brief.
async fn form_page(
    state: &AppState,
    s: &slath_core::authz::Subject,
    (ns, repo, n, change): (&str, &str, i32, &str),
    ctx: PageCtx,
    values: Option<(Slots, String, Vec<FieldError>, Option<String>)>,
) -> Result<Response, AppError> {
    let (a, m) = state.services.mr(s, ns, repo, n).await?;
    if !slath_service::Services::may_write_brief(s, &a, &m) {
        return Err(AppError::not_found());
    }
    let info = state.services.mr_change(&m, change).await?;
    let latest = info.versions.last().ok_or_else(AppError::not_found)?;
    let page_url = format!(
        "/{ns}/{repo}/merge-requests/{n}/changes/{}",
        info.display_id
    );
    let current = state
        .services
        .mr_change_briefs(&m, &info.display_id, Some(&latest.commit_oid), 1)
        .await?
        .into_iter()
        .next();
    let (slots, commit, errors, focus) = match values {
        Some(v) => v,
        None => {
            // Without a brief of its own, the latest version starts from
            // the newest earlier one's, carried items at their new places.
            let earlier = match &current {
                Some(_) => None,
                None => state
                    .services
                    .carried_brief(&m, &info.display_id, &latest.commit_oid)
                    .await?
                    .map(|c| c.items),
            };
            let items = current
                .as_ref()
                .map(|c| c.items.as_slice())
                .or(earlier.as_deref())
                .unwrap_or_default();
            let mut slots = Slots::of_items(items);
            // A new brief starts with what the change is for.
            if let Some((_, first)) = slots.0.first_mut()
                && first.is_empty()
            {
                first.push(Default::default());
            }
            (slots, latest.commit_oid.clone(), Vec::new(), None)
        }
    };
    let status = if errors.is_empty() {
        StatusCode::OK
    } else {
        StatusCode::UNPROCESSABLE_ENTITY
    };
    let short: String = info.display_id.chars().take(8).collect();
    let mut res = render(&FormPage {
        header: RepoHeader::load(state, &a, s, "merge-requests").await,
        heading: if current.is_some() {
            format!("Edit the brief of change {short}")
        } else {
            format!("Write a brief for change {short}")
        },
        action: format!("{page_url}/brief"),
        back: page_url,
        commit,
        groups: slots.groups(),
        focus,
        errors,
        ctx,
    });
    *res.status_mut() = status;
    Ok(res)
}

async fn show_form(
    State(state): State<AppState>,
    ctx: PageCtx,
    RequireSession(s): RequireSession,
    Path((ns, repo, n, change)): Path<(String, String, String, String)>,
) -> Result<Response, AppError> {
    let n: i32 = n.parse().map_err(|_| AppError::not_found())?;
    form_page(&state, &s.subject, (&ns, &repo, n, &change), ctx, None).await
}

/// The form field an error's JSON pointer (`/items/2/text`) names.
fn field_of(pointer: &str, ids: &[String]) -> String {
    let mut parts = pointer.trim_start_matches('/').split('/');
    match (
        parts.next(),
        parts.next().and_then(|i| i.parse::<usize>().ok()),
    ) {
        (Some("items"), Some(i)) if i < ids.len() => {
            let part = parts.next().unwrap_or("text");
            format!("{}-{part}", ids[i])
        }
        // No items at all: at the first one, what the change is for.
        (Some("items"), None) => "item-motivation-0-text".to_owned(),
        _ => pointer.trim_start_matches('/').to_owned(),
    }
}

async fn write(
    State(state): State<AppState>,
    ctx: PageCtx,
    RequireSession(s): RequireSession,
    Path((ns, repo, n, change)): Path<(String, String, String, String)>,
    Form(fields): Form<Vec<(String, String)>>,
) -> Result<Response, AppError> {
    let n: i32 = n.parse().map_err(|_| AppError::not_found())?;
    let field = |name: &str| {
        fields
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.clone())
            .unwrap_or_default()
    };
    let commit = field("commit");
    let mut slots = Slots::of_form(&fields);
    // "Add …" shows the form again with one more empty item, saving
    // nothing.
    if let Some(kind) = Kind::parse(&field("add")) {
        let mut focus = None;
        if let Some((_, items)) = slots.0.iter_mut().find(|(k, _)| *k == kind) {
            focus = Some(format!("item-{}-{}-text", kind.as_str(), items.len()));
            items.push(Default::default());
        }
        return form_page(
            &state,
            &s.subject,
            (&ns, &repo, n, &change),
            ctx,
            Some((slots, commit, Vec::new(), focus)),
        )
        .await;
    }
    let (items, ids) = slots.items();
    let body = serde_json::json!({
        "schema": SCHEMA,
        "items": items,
        // Opened on a version's commit; a form without one writes the
        // latest version's.
        "commit": (!commit.is_empty()).then_some(&commit),
    });
    let body = serde_json::to_vec(&body).map_err(|e| AppError::internal(&e))?;
    match state
        .services
        .write_change_brief(&s.subject, (&ns, &repo, n), &change, &body)
        .await
    {
        Ok(_) => Ok(Redirect::to(&format!(
            "/{ns}/{repo}/merge-requests/{n}/changes/{change}#brief"
        ))
        .into_response()),
        Err(ServiceError::Invalid(errors)) => {
            // The service names fields by JSON pointer; the form by id.
            let errors = errors
                .into_iter()
                .map(|e| FieldError {
                    field: field_of(&e.field, &ids),
                    ..e
                })
                .collect();
            // Keep an empty item where an error points at "no items".
            if let Some((_, first)) = slots.0.first_mut()
                && first.is_empty()
            {
                first.push(Default::default());
            }
            form_page(
                &state,
                &s.subject,
                (&ns, &repo, n, &change),
                ctx,
                Some((slots, commit, errors, None)),
            )
            .await
        }
        Err(e) => Err(e.into()),
    }
}

pub fn router() -> Router<AppState> {
    Router::new().route_as(
        "/{ns}/{repo}/merge-requests/{n}/changes/{change}/brief",
        Access::Resource,
        axum::routing::get(show_form).post(write),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn form_fields_become_items_in_kind_order() {
        let f = |k: &str, v: &str| (k.to_owned(), v.to_owned());
        let slots = Slots::of_form(&[
            f("item-effect-1-text", "second"),
            f("item-effect-0-text", "first"),
            f("item-effect-0-before", "slow"),
            f("item-motivation-0-text", "why"),
            f("item-risk-0-text", "  "),
            f("item-wish-0-text", "ignored"),
            f("commit", "x"),
        ]);
        let (items, ids) = slots.items();
        let texts: Vec<&str> = items.iter().map(|v| v["text"].as_str().unwrap()).collect();
        assert_eq!(texts, ["why", "first", "second"]);
        assert_eq!(items[1]["before"], "slow");
        assert_eq!(ids, ["item-motivation-0", "item-effect-0", "item-effect-1"]);
        assert_eq!(field_of("/items/2/text", &ids), "item-effect-1-text");
        assert_eq!(field_of("/items/1/before", &ids), "item-effect-0-before");
        assert_eq!(field_of("/items", &ids), "item-motivation-0-text");
        let names: Vec<String> = slots.groups()[1]
            .items
            .iter()
            .map(|s| s.name.clone())
            .collect();
        assert_eq!(names, ["Effect 1", "Effect 2"]);
        assert_eq!(slots.groups()[1].add, "Add an effect");
        assert_eq!(slots.groups()[2].add, "Add a risk");
    }
}
