// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Change briefs in the UI: the Brief section of a change's page (the
//! shown version's newest edit), the comparison of two versions' briefs,
//! and the page that writes one
//! (`…/merge-requests/{n}/changes/{change}/brief`). A brief is its
//! author's claim, shown as plain text.

use askama::Template;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Redirect, Response};
use axum::{Form, Router};
use serde::Deserialize;
use slath_db::change_briefs::Brief;
use slath_db::merge_requests::MergeRequest;
use slath_service::change_briefs::{ChangeBrief, SCHEMA};
use slath_service::{FieldError, ServiceError};

use super::repos::RepoHeader;
use super::settings::error_for;
use super::{PageCtx, Time, Who, render};
use crate::AppState;
use crate::auth::RequireSession;
use crate::error::AppError;
use crate::routes::{Access, RouteExt};

/// The fields in the order shown, with their names and form hints.
const FIELDS: [(&str, &str, &str); 4] = [
    ("intent", "Intent", "What the change is for."),
    ("approach", "Approach", "How it does it."),
    (
        "verification",
        "Verification",
        "How it was checked: tests run, what was tried.",
    ),
    ("gaps", "Known gaps", "What's known to be missing or wrong."),
];

/// A field's name as the page shows it.
pub fn field_label(field: &str) -> &'static str {
    FIELDS
        .iter()
        .find(|(id, _, _)| *id == field)
        .map_or("Brief", |(_, label, _)| label)
}

fn field_texts(b: &Brief) -> [Option<&str>; 4] {
    [
        Some(b.intent.as_str()),
        b.approach.as_deref(),
        b.verification.as_deref(),
        b.gaps.as_deref(),
    ]
}

/// A version's brief as the change's page shows it.
pub struct BriefView {
    pub id: String,
    pub author: Who,
    pub created: Time,
    /// How many edits the version's brief has (1 without edits).
    pub edits: usize,
    /// Each present field.
    pub fields: Vec<FieldView>,
}

/// One field of the shown brief, with the threads on it.
pub struct FieldView {
    pub id: &'static str,
    pub label: &'static str,
    pub text: String,
    /// Each thread's ID, starter, number of comments, and whether it's
    /// resolved.
    pub threads: Vec<(String, Who, usize, bool)>,
    /// Opens the comment form under the field, for those who may comment.
    pub comment_href: Option<String>,
    /// The comment form is open here.
    pub form_open: bool,
}

/// Who views the change's page, as the brief section needs it.
pub struct Viewer<'a> {
    pub subject: &'a slath_core::authz::Subject,
    /// May write briefs (the latest version's) and comment.
    pub write: bool,
    pub comment: bool,
    /// The field whose comment form is open (`?brief_comment=`).
    pub open: Option<&'a str>,
}

/// One field of two versions' briefs compared.
pub struct FieldDiff {
    pub label: &'static str,
    pub diff: Result<Vec<super::merge_requests::DescriptionLine>, ()>,
}

/// What the change's page shows of briefs.
pub struct BriefParts {
    /// The shown version's brief, when not comparing.
    pub shown: Option<BriefView>,
    /// The changed fields when comparing two versions.
    pub changed: Vec<FieldDiff>,
    /// The page that writes the brief of the latest version, for those
    /// who may, with whether one exists.
    pub write: Option<(String, bool)>,
    /// Where a field's comment form posts, and the page it returns to.
    pub threads_action: String,
    pub back: String,
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
        let shown = match of(shown) {
            None => None,
            Some(v) => {
                let threads = state
                    .services
                    .brief_threads(viewer.subject, m, v.brief.id)
                    .await?;
                Some(view(v, edits(shown), &threads, &page, viewer))
            }
        };
        return Ok(BriefParts {
            shown,
            changed: Vec::new(),
            write,
            threads_action: format!("/{ns}/{repo}/merge-requests/{}/threads", m.number),
            back: page,
        });
    };
    let empty = [None; 4];
    let a = of(from).map_or(empty, |v| field_texts(&v.brief));
    let b = of(shown).map_or(empty, |v| field_texts(&v.brief));
    let changed = FIELDS
        .iter()
        .zip(a.iter().zip(b.iter()))
        .filter_map(|((_, label, _), (x, y))| {
            super::merge_requests::description_diff(x.unwrap_or(""), y.unwrap_or(""))
                .map(|diff| FieldDiff { label, diff })
        })
        .collect();
    Ok(BriefParts {
        shown: None,
        changed,
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
        fields: FIELDS
            .iter()
            .zip(field_texts(&v.brief))
            .filter_map(|((id, label, _), text)| {
                let text = text?;
                Some(FieldView {
                    id,
                    label,
                    text: slath_markup::reveal_bidi(text).into_owned(),
                    threads: threads
                        .iter()
                        .filter(|t| t.field == *id)
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
                        .then(|| format!("{page}?brief_comment={id}#brief-{id}")),
                    form_open: viewer.comment && viewer.open == Some(*id),
                })
            })
            .collect(),
    }
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
    /// Each field's id, name, hint, and value.
    fields: Vec<(&'static str, &'static str, &'static str, String)>,
    errors: Vec<FieldError>,
}

impl FormPage {
    fn error(&self, field: &str) -> Option<&str> {
        error_for(&self.errors, field)
    }
}

#[derive(Deserialize)]
struct BriefForm {
    #[serde(default)]
    commit: String,
    #[serde(default)]
    intent: String,
    #[serde(default)]
    approach: String,
    #[serde(default)]
    verification: String,
    #[serde(default)]
    gaps: String,
}

/// The form page's frame for merge request `n`'s change `change`.
async fn form_page(
    state: &AppState,
    s: &slath_core::authz::Subject,
    (ns, repo, n, change): (&str, &str, i32, &str),
    ctx: PageCtx,
    values: Option<(&BriefForm, Vec<FieldError>)>,
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
    let (texts, commit, errors) = match values {
        Some((f, errors)) => (
            [
                f.intent.clone(),
                f.approach.clone(),
                f.verification.clone(),
                f.gaps.clone(),
            ],
            f.commit.clone(),
            errors,
        ),
        None => (
            current.as_ref().map_or_else(Default::default, |c| {
                field_texts(&c.brief).map(|t| t.unwrap_or_default().to_owned())
            }),
            latest.commit_oid.clone(),
            Vec::new(),
        ),
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
        fields: FIELDS
            .iter()
            .zip(texts)
            .map(|((id, label, hint), v)| (*id, *label, *hint, v))
            .collect(),
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

async fn write(
    State(state): State<AppState>,
    ctx: PageCtx,
    RequireSession(s): RequireSession,
    Path((ns, repo, n, change)): Path<(String, String, String, String)>,
    Form(f): Form<BriefForm>,
) -> Result<Response, AppError> {
    let n: i32 = n.parse().map_err(|_| AppError::not_found())?;
    let body = serde_json::json!({
        "schema": SCHEMA,
        "intent": f.intent,
        "approach": f.approach,
        "verification": f.verification,
        "gaps": f.gaps,
        // Opened on a version's commit; a form without one writes the
        // latest version's.
        "commit": (!f.commit.is_empty()).then_some(&f.commit),
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
                    field: e.field.trim_start_matches('/').to_owned(),
                    ..e
                })
                .collect();
            form_page(
                &state,
                &s.subject,
                (&ns, &repo, n, &change),
                ctx,
                Some((&f, errors)),
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
