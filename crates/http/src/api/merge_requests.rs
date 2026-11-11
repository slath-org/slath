// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! `/repos/{ns}/{repo}/merge-requests`: merge requests, their revisions,
//! and their comments.

use axum::extract::{OriginalUri, Path, Query, State};
use axum::http::StatusCode;
use axum::response::Response;
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use slath_markup::refs::RefContext;
use slath_service::issues::TimelineItem;
use slath_service::labels::FilterInput;
use slath_service::merge_requests::{MrTimelineItem, NewMr};
use slath_service::mr_changes::Rebind;
use utoipa::{IntoParams, ToSchema};

use super::support::{PageParams, page};
use super::{ApiSubject, ApiUser};
use crate::AppState;
use crate::error::AppError;
use crate::routes::{Access, RouteExt};

fn ts(t: time::OffsetDateTime) -> String {
    crate::ui::Time::from(t).iso
}

#[derive(Serialize, ToSchema)]
pub struct MergeRequest {
    pub number: i32,
    pub title: String,
    /// Markdown source.
    pub description: String,
    /// The description rendered for the caller.
    pub description_html: String,
    /// `open`, `merged`, or `closed`.
    pub state: String,
    pub draft: bool,
    /// `None` for deleted accounts.
    pub author: Option<String>,
    /// `ns/name` of the source repository: this repository or a fork in
    /// its network. `None` once that fork is deleted.
    pub source_repo: Option<String>,
    pub source_branch: String,
    pub target_branch: String,
    /// Fork merge requests: writers of this repository may push to the
    /// source branch while the merge request is open.
    pub allow_maintainer_edits: bool,
    /// Fork merge requests: a source head this repository's push protection
    /// held back. It isn't fetched, and the merge request keeps its previous
    /// revision until the findings are removed or allowed.
    pub held_source: Option<HeldSource>,
    /// Head commit of the current revision.
    pub head_oid: Option<String>,
    /// Number of revisions (pushes that changed the source).
    pub revisions: i32,
    pub comments: i64,
    pub created_at: String,
    pub updated_at: String,
    pub closed_at: Option<String>,
    pub merged_at: Option<String>,
    /// Merge when ready, if set.
    pub auto_merge: Option<AutoMergeJson>,
}

/// Merge when ready: it merges as `set_by` with `strategy` once every
/// requirement is met.
#[derive(Serialize, ToSchema)]
pub struct AutoMergeJson {
    pub strategy: String,
    /// `None` for deleted accounts.
    pub set_by: Option<String>,
    pub set_at: String,
}

#[derive(Serialize, ToSchema)]
pub struct HeldSource {
    /// The source head that wasn't fetched.
    pub head: String,
    /// Why the scan couldn't finish, if it couldn't.
    pub incomplete: Option<String>,
    pub findings: Vec<HeldFinding>,
}

/// A possible secret, redacted: the excerpt keeps four characters of it.
#[derive(Serialize, ToSchema)]
pub struct HeldFinding {
    pub rule: String,
    pub description: String,
    /// `file`, `commit_message`, or `tag_message`.
    pub place: String,
    /// The commit or tag object it's in.
    pub object: String,
    pub path: Option<String>,
    pub line: usize,
    pub excerpt: String,
    pub fingerprint: String,
}

fn held_json(h: slath_service::push_protection::HeldSource) -> HeldSource {
    HeldSource {
        head: h.head,
        incomplete: h.incomplete,
        findings: h
            .findings
            .into_iter()
            .map(|f| HeldFinding {
                rule: f.rule,
                description: f.description,
                place: f.place,
                object: f.object,
                path: f.path,
                line: f.line,
                excerpt: f.excerpt,
                fingerprint: f.fingerprint,
            })
            .collect(),
    }
}

fn mr_json(
    m: slath_db::merge_requests::MergeRequest,
    html: String,
    source_repo: Option<String>,
    auto_merge: Option<slath_db::merge_requests::AutoMerge>,
) -> MergeRequest {
    MergeRequest {
        auto_merge: auto_merge.map(|a| AutoMergeJson {
            strategy: a.strategy,
            set_by: a.by,
            set_at: ts(a.at),
        }),
        held_source: slath_service::push_protection::HeldSource::of(&m).map(held_json),
        source_repo,
        allow_maintainer_edits: m.allow_maintainer_edits,
        source_branch: m.source_branch(),
        number: m.number,
        title: m.title,
        description: m.description,
        description_html: html,
        state: m.state,
        draft: m.draft,
        author: m.author,
        target_branch: m.target_branch,
        head_oid: m.head_oid,
        revisions: m.revisions,
        comments: m.comments,
        created_at: ts(m.created_at),
        updated_at: ts(m.updated_at),
        closed_at: m.closed_at.map(ts),
        merged_at: m.merged_at.map(ts),
    }
}

async fn render(
    state: &AppState,
    s: &slath_core::authz::Subject,
    ns: &str,
    repo: &str,
    m: slath_db::merge_requests::MergeRequest,
) -> Result<MergeRequest, AppError> {
    let ctx = RefContext {
        ns: ns.to_owned(),
        repo: repo.to_owned(),
    };
    let html = state
        .services
        .render_markdown(s, Some(&ctx), &m.description)
        .await?;
    let source = state.services.mr_source_name(&m).await?;
    let auto_merge = state.services.mr_auto_merge(&m).await?;
    Ok(mr_json(m, html, source, auto_merge))
}

#[derive(Serialize, ToSchema)]
pub struct MrPage {
    pub items: Vec<MergeRequest>,
    pub next_cursor: Option<String>,
}

#[derive(Deserialize, IntoParams)]
pub struct MrQuery {
    /// `open` (default), `merged`, `closed`, or `all`.
    pub state: Option<String>,
    /// `created` (default) or `updated`.
    pub sort: Option<String>,
    /// Comma-separated label names; merge requests must have all of them.
    pub labels: Option<String>,
    /// Username.
    pub assignee: Option<String>,
    /// Username.
    pub author: Option<String>,
    /// Milestone title.
    pub milestone: Option<String>,
}

/// A repository's merge requests, newest first.
#[utoipa::path(get, path = "/api/v1/repos/{ns}/{repo}/merge-requests", tag = "merge requests", security((), ("token" = ["repo:read"])), params(("ns" = String, Path), ("repo" = String, Path), MrQuery, PageParams),
    responses((status = 200, body = MrPage), (status = 404, body = super::Problem)))]
pub async fn list_mrs(
    State(state): State<AppState>,
    ApiSubject(s): ApiSubject,
    Path((ns, repo)): Path<(String, String)>,
    OriginalUri(uri): OriginalUri,
    Query(q): Query<MrQuery>,
    Query(p): Query<PageParams>,
) -> Result<Response, AppError> {
    let (offset, limit) = (p.offset(&state)?, p.limit(state.config.limits.api_page_max));
    let filter = FilterInput {
        state: Some(q.state.unwrap_or_else(|| "open".to_owned())),
        sort_updated: q.sort.as_deref() == Some("updated"),
        labels: q
            .labels
            .as_deref()
            .unwrap_or("")
            .split(',')
            .filter(|l| !l.is_empty())
            .map(str::to_owned)
            .collect(),
        assignee: q.assignee,
        author: q.author,
        milestone: q.milestone,
    };
    let (_, items, _) = state
        .services
        .mr_list(&s, &ns, &repo, &filter, offset as i64, limit as i64 + 1)
        .await?;
    let mut out = Vec::new();
    for m in items {
        out.push(render(&state, &s, &ns, &repo, m).await?);
    }
    Ok(page(&state, &uri, out, offset, limit))
}

#[derive(Deserialize, ToSchema)]
pub struct NewMergeRequest {
    /// `ns/name` of a repository in this repository's fork network that
    /// you can push to. Default: this repository.
    #[serde(default)]
    pub source_repo: String,
    #[serde(default)]
    pub source_branch: String,
    /// Default: the repository's default branch.
    pub target_branch: Option<String>,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub draft: bool,
    /// Usernames.
    #[serde(default)]
    pub reviewers: Vec<String>,
    /// Usernames.
    #[serde(default)]
    pub assignees: Vec<String>,
    /// Label names.
    #[serde(default)]
    pub labels: Vec<String>,
    /// Milestone title.
    pub milestone: Option<String>,
    /// Default: the repository's setting.
    pub delete_source_on_merge: Option<bool>,
    /// Fork merge requests: let writers of this repository push to the
    /// source branch while the merge request is open. Default: true.
    pub allow_maintainer_edits: Option<bool>,
    /// Required when the source repository is less visible than this one:
    /// confirms that its commits become readable to this repository's
    /// readers.
    #[serde(default)]
    pub publish_source: bool,
}

/// Opens a merge request from a branch of this repository or of a fork in
/// its network.
#[utoipa::path(post, path = "/api/v1/repos/{ns}/{repo}/merge-requests", tag = "merge requests", security(("token" = ["mrs:write"])), params(("ns" = String, Path), ("repo" = String, Path)), request_body = NewMergeRequest,
    responses((status = 201, body = MergeRequest), (status = 401, body = super::Problem), (status = 403, body = super::Problem), (status = 404, body = super::Problem), (status = 422, body = super::Problem)))]
pub async fn create_mr(
    State(state): State<AppState>,
    ApiUser(s): ApiUser,
    Path((ns, repo)): Path<(String, String)>,
    Json(b): Json<NewMergeRequest>,
) -> Result<(StatusCode, Json<MergeRequest>), AppError> {
    let m = state
        .services
        .create_mr(
            &s,
            &ns,
            &repo,
            &NewMr {
                source_repo: b.source_repo,
                allow_maintainer_edits: b.allow_maintainer_edits,
                publish_source: b.publish_source,
                source_branch: b.source_branch,
                target_branch: b.target_branch.unwrap_or_default(),
                title: b.title,
                description: b.description,
                draft: b.draft,
                reviewers: b.reviewers,
                assignees: b.assignees,
                labels: b.labels,
                milestone: b.milestone,
                delete_source_on_merge: b.delete_source_on_merge,
            },
        )
        .await?;
    Ok((
        StatusCode::CREATED,
        Json(render(&state, &s, &ns, &repo, m).await?),
    ))
}

/// One merge request.
#[utoipa::path(get, path = "/api/v1/repos/{ns}/{repo}/merge-requests/{number}", tag = "merge requests", security((), ("token" = ["repo:read"])), params(("ns" = String, Path), ("repo" = String, Path), ("number" = i32, Path)),
    responses((status = 200, body = MergeRequest), (status = 404, body = super::Problem)))]
pub async fn get_mr(
    State(state): State<AppState>,
    ApiSubject(s): ApiSubject,
    Path((ns, repo, number)): Path<(String, String, i32)>,
) -> Result<Json<MergeRequest>, AppError> {
    let (_, m) = state.services.mr(&s, &ns, &repo, number).await?;
    Ok(Json(render(&state, &s, &ns, &repo, m).await?))
}

#[derive(Deserialize, ToSchema)]
pub struct UpdateMergeRequest {
    pub title: Option<String>,
    pub description: Option<String>,
    /// `open` or `closed`.
    pub state: Option<String>,
    pub draft: Option<bool>,
    /// Fork merge requests: let writers of this repository push to the
    /// source branch. Only someone who can push to the source may change it.
    pub allow_maintainer_edits: Option<bool>,
}

/// Edits, closes or reopens, or marks a merge request draft or ready
/// (author or triage); turns edits by maintainers on or off (whoever can
/// push to a fork source).
#[utoipa::path(patch, path = "/api/v1/repos/{ns}/{repo}/merge-requests/{number}", tag = "merge requests", security(("token" = ["mrs:write"])), params(("ns" = String, Path), ("repo" = String, Path), ("number" = i32, Path)), request_body = UpdateMergeRequest,
    responses((status = 200, body = MergeRequest), (status = 401, body = super::Problem), (status = 403, body = super::Problem), (status = 404, body = super::Problem), (status = 422, body = super::Problem)))]
pub async fn update_mr(
    State(state): State<AppState>,
    ApiUser(s): ApiUser,
    Path((ns, repo, number)): Path<(String, String, i32)>,
    Json(b): Json<UpdateMergeRequest>,
) -> Result<Json<MergeRequest>, AppError> {
    let (_, cur) = state.services.mr(&s, &ns, &repo, number).await?;
    if b.title.is_some() || b.description.is_some() {
        let title = b.title.unwrap_or(cur.title);
        let description = b.description.unwrap_or(cur.description);
        state
            .services
            .edit_mr(&s, &ns, &repo, number, &title, &description)
            .await?;
    }
    if let Some(d) = b.draft {
        state
            .services
            .set_mr_draft(&s, &ns, &repo, number, d)
            .await?;
    }
    if let Some(allow) = b.allow_maintainer_edits {
        state
            .services
            .set_maintainer_edits(&s, &ns, &repo, number, allow)
            .await?;
    }
    if let Some(st) = b.state.as_deref() {
        let open = match st {
            "open" => true,
            "closed" => false,
            _ => {
                return Err(slath_service::ServiceError::invalid(
                    "state",
                    "invalid",
                    "Use open or closed.",
                )
                .into());
            }
        };
        state
            .services
            .set_mr_open(&s, &ns, &repo, number, open)
            .await?;
    }
    let (_, m) = state.services.mr(&s, &ns, &repo, number).await?;
    Ok(Json(render(&state, &s, &ns, &repo, m).await?))
}

#[derive(Serialize, ToSchema)]
pub struct MrComment {
    pub id: String,
    pub author: Option<String>,
    pub body: String,
    pub body_html: String,
    pub created_at: String,
    pub edited_at: Option<String>,
}

/// The conversation's comments, oldest first.
#[utoipa::path(get, path = "/api/v1/repos/{ns}/{repo}/merge-requests/{number}/comments", tag = "merge requests", security((), ("token" = ["repo:read"])), params(("ns" = String, Path), ("repo" = String, Path), ("number" = i32, Path)),
    responses((status = 200, body = Vec<MrComment>), (status = 404, body = super::Problem)))]
pub async fn list_mr_comments(
    State(state): State<AppState>,
    ApiSubject(s): ApiSubject,
    Path((ns, repo, number)): Path<(String, String, i32)>,
) -> Result<Json<Vec<MrComment>>, AppError> {
    let (a, m) = state.services.mr(&s, &ns, &repo, number).await?;
    let items = state
        .services
        .mr_timeline(&s, &a, &m)
        .await?
        .into_iter()
        .filter_map(|i| match i {
            MrTimelineItem::Comment(TimelineItem::Comment {
                comment,
                html,
                hidden,
                ..
            }) if comment.deleted_at.is_none() && !(hidden && html.is_empty()) => Some(MrComment {
                id: comment.id.to_string(),
                author: comment.author,
                body: comment.body,
                body_html: html,
                created_at: ts(comment.created_at),
                edited_at: comment.edited_at.map(ts),
            }),
            _ => None,
        })
        .collect();
    Ok(Json(items))
}

#[derive(Deserialize, ToSchema)]
pub struct MrCommentBody {
    #[serde(default)]
    pub body: String,
}

/// Comments on a merge request.
#[utoipa::path(post, path = "/api/v1/repos/{ns}/{repo}/merge-requests/{number}/comments", tag = "merge requests", security(("token" = ["mrs:write"])), params(("ns" = String, Path), ("repo" = String, Path), ("number" = i32, Path)), request_body = MrCommentBody,
    responses((status = 201, body = MrComment), (status = 401, body = super::Problem), (status = 403, body = super::Problem), (status = 404, body = super::Problem), (status = 422, body = super::Problem)))]
pub async fn create_mr_comment(
    State(state): State<AppState>,
    ApiUser(s): ApiUser,
    Path((ns, repo, number)): Path<(String, String, i32)>,
    Json(b): Json<MrCommentBody>,
) -> Result<(StatusCode, Json<MrComment>), AppError> {
    let c = state
        .services
        .add_mr_comment(&s, &ns, &repo, number, &b.body)
        .await?;
    let ctx = RefContext {
        ns: ns.clone(),
        repo: repo.clone(),
    };
    let html = state
        .services
        .render_markdown(&s, Some(&ctx), &c.body)
        .await?;
    Ok((
        StatusCode::CREATED,
        Json(MrComment {
            id: c.id.to_string(),
            author: c.author,
            body: c.body,
            body_html: html,
            created_at: ts(c.created_at),
            edited_at: c.edited_at.map(ts),
        }),
    ))
}

/// One commit of the current revision and the change it belongs to.
#[derive(Serialize, ToSchema)]
pub struct StackEntry {
    /// 0 is the bottom of the stack.
    pub position: i32,
    pub commit_oid: String,
    /// The change's ID in jj's format; null while unresolved.
    pub change_id: Option<String>,
    /// `new`, `confirmed`, `inferred`, or `unresolved`.
    pub binding: String,
    /// `alias`, `patch`, `content`, `aliases-conflict`, `duplicate-id`,
    /// `split`, `join`, or `resolved`.
    pub reason: Option<String>,
    /// Changes it may continue (new) or must pick from (unresolved).
    pub candidates: Vec<String>,
    /// jj recorded a conflict in this commit.
    pub conflicted: bool,
    /// Versions of its change in earlier revisions.
    pub earlier_versions: usize,
    /// Other open merge requests of the repository with one of its IDs.
    pub also_in: Vec<i32>,
}

/// The changes of a merge request's current revision.
#[derive(Serialize, ToSchema)]
pub struct MrStack {
    /// The current revision; null before the first.
    pub revision: Option<i32>,
    /// Empty when the revision can only be reviewed as a whole.
    pub entries: Vec<StackEntry>,
    /// Changes without a version in the current revision.
    pub removed: Vec<String>,
    /// An unresolved or conflicted commit blocks merging.
    pub merge_blocked: bool,
}

/// The changes of the current revision: each commit with its change ID
/// and how it was matched.
#[utoipa::path(get, path = "/api/v1/repos/{ns}/{repo}/merge-requests/{number}/stack", tag = "merge requests", security((), ("token" = ["repo:read"])), params(("ns" = String, Path), ("repo" = String, Path), ("number" = i32, Path)),
    responses((status = 200, body = MrStack), (status = 404, body = super::Problem)))]
pub async fn get_stack(
    State(state): State<AppState>,
    ApiSubject(s): ApiSubject,
    Path((ns, repo, number)): Path<(String, String, i32)>,
) -> Result<Json<MrStack>, AppError> {
    let (_, m) = state.services.mr(&s, &ns, &repo, number).await?;
    let st = state.services.mr_stack(&m).await?;
    Ok(Json(MrStack {
        revision: st.revision,
        merge_blocked: st.blocks_merge(),
        removed: st.removed,
        entries: st
            .entries
            .into_iter()
            .map(|e| StackEntry {
                position: e.position,
                commit_oid: e.commit_oid,
                change_id: e.display_id,
                binding: e.binding,
                reason: e.reason,
                candidates: e.candidates,
                conflicted: e.conflicted,
                earlier_versions: e.earlier_versions,
                also_in: e.also_in,
            })
            .collect(),
    }))
}

#[derive(Deserialize, ToSchema)]
pub struct BindBody {
    /// The change to continue (its ID), or null for a new change.
    #[serde(default)]
    pub change: Option<String>,
}

/// Binds a commit of the current revision by hand: picks a change for an
/// unresolved commit, splits a continued change into a new one
/// (`change: null`), or joins a new change to a removed one. Open to the
/// author and anyone who can push the source; audited.
#[utoipa::path(post, path = "/api/v1/repos/{ns}/{repo}/merge-requests/{number}/stack/{position}/bind", tag = "merge requests", security(("token" = ["mrs:write"])), params(("ns" = String, Path), ("repo" = String, Path), ("number" = i32, Path), ("position" = i32, Path)), request_body = BindBody,
    responses((status = 200, body = MrStack), (status = 401, body = super::Problem), (status = 403, body = super::Problem), (status = 404, body = super::Problem), (status = 409, body = super::Problem), (status = 422, body = super::Problem)))]
pub async fn bind_change(
    State(state): State<AppState>,
    ApiUser(s): ApiUser,
    Path((ns, repo, number, position)): Path<(String, String, i32, i32)>,
    Json(b): Json<BindBody>,
) -> Result<Json<MrStack>, AppError> {
    let to = b.change.map_or(Rebind::New, Rebind::Change);
    state
        .services
        .rebind_change(&s, (&ns, &repo, number), position, &to)
        .await?;
    get_stack(State(state), ApiSubject(s), Path((ns, repo, number))).await
}

/// A review of a merge request.
#[derive(Serialize, ToSchema)]
pub struct MrReview {
    pub id: String,
    /// The revision it was submitted on.
    pub revision: i32,
    pub reviewer: Option<String>,
    /// `approve`, `request_changes`, or `comment`.
    pub verdict: String,
    pub body: String,
    pub created_at: String,
    /// The reviewer's current verdict (on this revision, or carried over a
    /// rebase that kept every patch).
    pub counts: bool,
    /// An approval or request for changes made stale by later changes.
    pub stale: bool,
}

impl From<slath_service::mr_reviews::ReviewView> for MrReview {
    fn from(v: slath_service::mr_reviews::ReviewView) -> Self {
        Self {
            id: v.review.id.to_string(),
            revision: v.review.revision_seq,
            reviewer: v.review.reviewer,
            verdict: v.review.verdict,
            body: v.review.body,
            created_at: ts(v.review.created_at),
            counts: v.counts,
            stale: v.stale,
        }
    }
}

/// The reviews of a merge request, oldest first.
#[utoipa::path(get, path = "/api/v1/repos/{ns}/{repo}/merge-requests/{number}/reviews", tag = "merge requests", security((), ("token" = ["repo:read"])), params(("ns" = String, Path), ("repo" = String, Path), ("number" = i32, Path)),
    responses((status = 200, body = Vec<MrReview>), (status = 404, body = super::Problem)))]
pub async fn list_reviews(
    State(state): State<AppState>,
    ApiSubject(s): ApiSubject,
    Path((ns, repo, number)): Path<(String, String, i32)>,
) -> Result<Json<Vec<MrReview>>, AppError> {
    let (_, m) = state.services.mr(&s, &ns, &repo, number).await?;
    let reviews = state.services.mr_reviews(&m).await?;
    Ok(Json(reviews.into_iter().map(Into::into).collect()))
}

#[derive(Deserialize, ToSchema)]
pub struct NewReview {
    /// `approve`, `request_changes`, or `comment` (default).
    #[serde(default = "comment")]
    pub verdict: String,
    #[serde(default)]
    pub body: String,
}

fn comment() -> String {
    "comment".to_owned()
}

/// Reviews the current revision. The author may only comment; a comment
/// needs a body.
#[utoipa::path(post, path = "/api/v1/repos/{ns}/{repo}/merge-requests/{number}/reviews", tag = "merge requests", security(("token" = ["mrs:write"])), params(("ns" = String, Path), ("repo" = String, Path), ("number" = i32, Path)), request_body = NewReview,
    responses((status = 201, body = MrReview), (status = 401, body = super::Problem), (status = 403, body = super::Problem), (status = 404, body = super::Problem), (status = 409, body = super::Problem), (status = 422, body = super::Problem)))]
pub async fn create_review(
    State(state): State<AppState>,
    ApiUser(s): ApiUser,
    Path((ns, repo, number)): Path<(String, String, i32)>,
    Json(b): Json<NewReview>,
) -> Result<(StatusCode, Json<MrReview>), AppError> {
    let r = state
        .services
        .submit_review(&s, (&ns, &repo, number), &b.verdict, &b.body)
        .await?;
    let (_, m) = state.services.mr(&s, &ns, &repo, number).await?;
    let v = state
        .services
        .mr_reviews(&m)
        .await?
        .into_iter()
        .find(|v| v.review.id == r.id)
        .ok_or_else(AppError::not_found)?;
    Ok((StatusCode::CREATED, Json(v.into())))
}

#[derive(Serialize, Deserialize, ToSchema)]
pub struct Reviewers {
    /// User names, and `org/team` for teams of the repository's
    /// organization.
    #[serde(default)]
    pub reviewers: Vec<String>,
}

/// Sets who is asked to review (the author or triagers).
#[utoipa::path(put, path = "/api/v1/repos/{ns}/{repo}/merge-requests/{number}/reviewers", tag = "merge requests", security(("token" = ["mrs:write"])), params(("ns" = String, Path), ("repo" = String, Path), ("number" = i32, Path)), request_body = Reviewers,
    responses((status = 200, body = Reviewers), (status = 401, body = super::Problem), (status = 403, body = super::Problem), (status = 404, body = super::Problem), (status = 422, body = super::Problem)))]
pub async fn put_reviewers(
    State(state): State<AppState>,
    ApiUser(s): ApiUser,
    Path((ns, repo, number)): Path<(String, String, i32)>,
    Json(b): Json<Reviewers>,
) -> Result<Json<Reviewers>, AppError> {
    state
        .services
        .set_mr_reviewers(&s, (&ns, &repo, number), &b.reviewers)
        .await?;
    let (_, m) = state.services.mr(&s, &ns, &repo, number).await?;
    let (_, reviewers, _, _) = state.services.mr_details(&m).await?;
    Ok(Json(Reviewers {
        reviewers: reviewers.into_iter().map(|(n, _)| n).collect(),
    }))
}

/// One requirement of the merge box.
#[derive(Serialize, ToSchema)]
pub struct Requirement {
    /// `open`, `draft`, `approvals`, `changes_requested`, `up_to_date`,
    /// `threads`, or `identity`.
    pub key: String,
    /// `met`, `unmet`, or `pending`.
    pub state: String,
    pub reason: String,
    /// What fixes it.
    pub fix_action: Option<String>,
}

/// The merge box as data.
#[derive(Serialize, ToSchema)]
pub struct MergeabilityBody {
    pub mergeable: bool,
    pub requirements: Vec<Requirement>,
    /// The source contains the target's head; if not, `update-branch`
    /// brings it up to date.
    pub up_to_date: bool,
}

/// Whether the merge request can be merged, with each requirement.
#[utoipa::path(get, path = "/api/v1/repos/{ns}/{repo}/merge-requests/{number}/mergeability", tag = "merge requests", security((), ("token" = ["repo:read"])), params(("ns" = String, Path), ("repo" = String, Path), ("number" = i32, Path)),
    responses((status = 200, body = MergeabilityBody), (status = 404, body = super::Problem)))]
pub async fn get_mergeability(
    State(state): State<AppState>,
    ApiSubject(s): ApiSubject,
    Path((ns, repo, number)): Path<(String, String, i32)>,
) -> Result<Json<MergeabilityBody>, AppError> {
    let (a, m) = state.services.mr(&s, &ns, &repo, number).await?;
    let mb = state.services.mr_mergeability(&a, &m).await?;
    Ok(Json(MergeabilityBody {
        mergeable: mb.mergeable,
        up_to_date: mb.up_to_date,
        requirements: mb
            .requirements
            .into_iter()
            .map(|r| Requirement {
                key: r.key.to_owned(),
                state: r.state.to_owned(),
                reason: r.reason,
                fix_action: r.fix,
            })
            .collect(),
    }))
}

/// Every field is optional in the JSON so that authorization runs first.
#[derive(Deserialize, ToSchema)]
pub struct MergeBody {
    /// `fast-forward`, `rebase`, `merge-commit`, or `squash`; default: the
    /// repository's default.
    #[serde(default)]
    pub strategy: Option<String>,
    /// The head you reviewed; if the merge request moved on, 409.
    #[serde(default)]
    pub expected_head_oid: String,
    /// For merge commits and squashes.
    #[serde(default)]
    pub commit_message: Option<String>,
    /// Default: the merge request's setting.
    #[serde(default)]
    pub delete_source: Option<bool>,
}

/// Merges the merge request (`mr.merge`). 409 when its head isn't
/// `expected_head_oid`, a requirement isn't met, merging conflicts, or the
/// target kept moving.
#[utoipa::path(post, path = "/api/v1/repos/{ns}/{repo}/merge-requests/{number}/merge", tag = "merge requests", security(("token" = ["mrs:write"])), params(("ns" = String, Path), ("repo" = String, Path), ("number" = i32, Path)), request_body = MergeBody,
    responses((status = 200, body = MergeRequest), (status = 401, body = super::Problem), (status = 403, body = super::Problem), (status = 404, body = super::Problem), (status = 409, body = super::Problem), (status = 422, body = super::Problem)))]
pub async fn merge(
    State(state): State<AppState>,
    ApiUser(s): ApiUser,
    Path((ns, repo, number)): Path<(String, String, i32)>,
    Json(b): Json<MergeBody>,
) -> Result<Json<MergeRequest>, AppError> {
    let m = state
        .services
        .merge_mr(
            &s,
            (&ns, &repo, number),
            &slath_service::merge::MergeInput {
                strategy: b.strategy,
                expected_head_oid: b.expected_head_oid,
                commit_message: b.commit_message,
                delete_source: b.delete_source,
            },
        )
        .await?;
    Ok(Json(render(&state, &s, &ns, &repo, m).await?))
}

/// One file of a diff inventory.
#[derive(Serialize, ToSchema)]
pub struct InventoryFile {
    pub path: String,
    /// For renames and copies.
    pub old_path: Option<String>,
    /// `content`, `rename`, `copy`, `mode_only`, `whitespace_only`,
    /// `generated`, `binary`, `symlink`, `submodule`, or `not_shown`.
    pub class: String,
    /// Why it's in its class (for `not_shown`, the limit by name).
    pub reason: String,
    /// For `content` and `whitespace_only` files, each hunk's class:
    /// `content` or `whitespace_only`.
    pub hunks: Vec<String>,
    /// Folded in diff views (every class but `content`).
    pub folded: bool,
}

/// A diff's inventory: every file in exactly one class.
#[derive(Serialize, ToSchema)]
pub struct InventoryBody {
    pub files: Vec<InventoryFile>,
    /// Files changed but not listed, past `files_limit`.
    pub left_out: usize,
    /// The limit the list stopped at (`diff_max_files`), if it did.
    pub files_limit: Option<String>,
}

impl From<slath_git::inventory::Inventory> for InventoryBody {
    fn from(i: slath_git::inventory::Inventory) -> Self {
        Self {
            files: i
                .files
                .into_iter()
                .map(|f| InventoryFile {
                    path: String::from_utf8_lossy(&f.path).into_owned(),
                    old_path: f.old_path.map(|p| String::from_utf8_lossy(&p).into_owned()),
                    class: f.class.as_str().to_owned(),
                    reason: f.reason,
                    hunks: f.hunks.iter().map(|h| h.as_str().to_owned()).collect(),
                    folded: f.class.folded(),
                })
                .collect(),
            left_out: i.left_out,
            files_limit: i.files_limit.map(str::to_owned),
        }
    }
}

#[derive(Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct InventoryQuery {
    /// The revision (default: the current one).
    pub revision: Option<i32>,
    /// A change version's commit in that revision: its own diff.
    pub commit: Option<String>,
}

/// The inventory of a merge request revision's diff (`merge_base..head`),
/// or of one change version's diff (`parent..commit`).
#[utoipa::path(get, path = "/api/v1/repos/{ns}/{repo}/merge-requests/{number}/inventory", tag = "merge requests", security((), ("token" = ["repo:read"])), params(("ns" = String, Path), ("repo" = String, Path), ("number" = i32, Path), InventoryQuery),
    responses((status = 200, body = InventoryBody), (status = 404, body = super::Problem)))]
pub async fn get_inventory(
    State(state): State<AppState>,
    ApiSubject(s): ApiSubject,
    Path((ns, repo, number)): Path<(String, String, i32)>,
    Query(q): Query<InventoryQuery>,
) -> Result<Json<InventoryBody>, AppError> {
    let inv = state
        .services
        .mr_inventory(&s, (&ns, &repo, number), q.revision, q.commit.as_deref())
        .await?;
    Ok(Json(inv.into()))
}

/// Where to fetch a merge request as a branch.
#[derive(Serialize, ToSchema)]
pub struct Checkout {
    /// `mr/<n>` for merge requests from forks, else the source branch.
    pub branch: String,
    /// A URL that always authenticates (fetches of `mr/<n>` need it over
    /// HTTPS on public and internal repositories).
    pub fetch_url: String,
    /// `refs/merge-requests/<n>/head`, for scripts.
    pub head_ref: String,
}

/// "Check out": a fork merge request's branch `mr/<n>` shows in the
/// caller's fetches of this repository from now on.
#[utoipa::path(post, path = "/api/v1/repos/{ns}/{repo}/merge-requests/{number}/checkout", tag = "merge requests", security(("token" = ["repo:read"])), params(("ns" = String, Path), ("repo" = String, Path), ("number" = i32, Path)),
    responses((status = 200, body = Checkout), (status = 401, body = super::Problem), (status = 404, body = super::Problem)))]
pub async fn checkout(
    State(state): State<AppState>,
    ApiUser(s): ApiUser,
    Path((ns, repo, number)): Path<(String, String, i32)>,
) -> Result<Json<Checkout>, AppError> {
    let (a, m) = state.services.mr(&s, &ns, &repo, number).await?;
    state.services.note_checkout(&s, &m).await?;
    let fork = m.source_repo_id.is_some_and(|r| r != a.repo.id);
    Ok(Json(Checkout {
        branch: if fork {
            format!("mr/{number}")
        } else {
            m.source_branch()
        },
        fetch_url: state
            .config
            .server
            .public_url
            .join(&format!("/-/auth/{}/{}.git", a.repo.namespace, a.repo.name)),
        head_ref: format!("refs/merge-requests/{number}/head"),
    }))
}

/// Every field is optional in the JSON so that authorization runs first.
#[derive(Deserialize, ToSchema)]
pub struct ApplySuggestionsBody {
    /// IDs of published line comments with a ```` ```suggestion ```` block.
    #[serde(default)]
    pub comments: Vec<String>,
    /// `commit` (default): a new commit on top; `fold`: into the change
    /// that introduced the lines.
    #[serde(default)]
    pub mode: String,
    /// The head you saw; if the merge request moved on, 409.
    #[serde(default)]
    pub expected_head_oid: Option<String>,
}

/// Applies suggestions to the source branch (`mr.apply_suggestion`, or
/// the merge request's author; writing the branch needs push on the source
/// or edits by maintainers). 409 when one is outdated, two overlap, or the
/// branch moved or is protected.
#[utoipa::path(post, path = "/api/v1/repos/{ns}/{repo}/merge-requests/{number}/suggestions/apply", tag = "merge requests", security(("token" = ["mrs:write", "repo:write"])), params(("ns" = String, Path), ("repo" = String, Path), ("number" = i32, Path)), request_body = ApplySuggestionsBody,
    responses((status = 200, body = MergeRequest), (status = 401, body = super::Problem), (status = 403, body = super::Problem), (status = 404, body = super::Problem), (status = 409, body = super::Problem), (status = 422, body = super::Problem)))]
pub async fn apply_suggestions(
    State(state): State<AppState>,
    ApiUser(s): ApiUser,
    Path((ns, repo, number)): Path<(String, String, i32)>,
    Json(b): Json<ApplySuggestionsBody>,
) -> Result<Json<MergeRequest>, AppError> {
    let ids: Vec<uuid::Uuid> = b
        .comments
        .iter()
        .map(|c| c.parse().map_err(|_| AppError::not_found()))
        .collect::<Result<_, _>>()?;
    let mode = slath_service::suggestions::ApplyMode::parse(&b.mode)?;
    let m = state
        .services
        .apply_suggestions(
            &s,
            (&ns, &repo, number),
            &ids,
            mode,
            b.expected_head_oid.as_deref(),
        )
        .await?;
    Ok(Json(render(&state, &s, &ns, &repo, m).await?))
}

/// Every field is optional in the JSON so that authorization runs first.
#[derive(Deserialize, ToSchema)]
pub struct AutoMergeBody {
    /// Default: the repository's default strategy.
    #[serde(default)]
    pub strategy: Option<String>,
}

/// Sets merge when ready (`mr.auto_merge`): it merges as you, with
/// `strategy`, as soon as every requirement is met (right away if they
/// are). A new revision pushed by someone else cancels it.
#[utoipa::path(put, path = "/api/v1/repos/{ns}/{repo}/merge-requests/{number}/auto-merge", tag = "merge requests", security(("token" = ["mrs:write"])), params(("ns" = String, Path), ("repo" = String, Path), ("number" = i32, Path)), request_body = AutoMergeBody,
    responses((status = 200, body = MergeRequest), (status = 401, body = super::Problem), (status = 403, body = super::Problem), (status = 404, body = super::Problem), (status = 409, body = super::Problem), (status = 422, body = super::Problem)))]
pub async fn put_auto_merge(
    State(state): State<AppState>,
    ApiUser(s): ApiUser,
    Path((ns, repo, number)): Path<(String, String, i32)>,
    Json(b): Json<AutoMergeBody>,
) -> Result<Json<MergeRequest>, AppError> {
    let m = state
        .services
        .set_auto_merge(&s, (&ns, &repo, number), b.strategy.as_deref())
        .await?;
    Ok(Json(render(&state, &s, &ns, &repo, m).await?))
}

/// Cancels merge when ready: whoever set it, the author, or anyone who may
/// set it.
#[utoipa::path(delete, path = "/api/v1/repos/{ns}/{repo}/merge-requests/{number}/auto-merge", tag = "merge requests", security(("token" = ["mrs:write"])), params(("ns" = String, Path), ("repo" = String, Path), ("number" = i32, Path)),
    responses((status = 204), (status = 401, body = super::Problem), (status = 403, body = super::Problem), (status = 404, body = super::Problem)))]
pub async fn delete_auto_merge(
    State(state): State<AppState>,
    ApiUser(s): ApiUser,
    Path((ns, repo, number)): Path<(String, String, i32)>,
) -> Result<StatusCode, AppError> {
    state
        .services
        .cancel_auto_merge(&s, (&ns, &repo, number))
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Every field is optional in the JSON so that authorization runs first.
#[derive(Deserialize, ToSchema)]
pub struct UpdateBranchBody {
    /// `rebase` (onto the target, keeping headers and change IDs) or
    /// `merge` (the target into the branch).
    #[serde(default)]
    pub method: String,
    /// The head you saw; if the merge request moved on, 409.
    #[serde(default)]
    pub expected_head_oid: Option<String>,
}

/// Brings the source branch up to date with the target
/// (`mr.update_branch`): needs push on the source, or edits by maintainers
/// of a fork's branch. The new head becomes a revision. 409 when it's
/// already up to date, conflicts, moved, or its protection refuses.
#[utoipa::path(post, path = "/api/v1/repos/{ns}/{repo}/merge-requests/{number}/update-branch", tag = "merge requests", security(("token" = ["mrs:write", "repo:write"])), params(("ns" = String, Path), ("repo" = String, Path), ("number" = i32, Path)), request_body = UpdateBranchBody,
    responses((status = 200, body = MergeRequest), (status = 401, body = super::Problem), (status = 403, body = super::Problem), (status = 404, body = super::Problem), (status = 409, body = super::Problem), (status = 422, body = super::Problem)))]
pub async fn update_branch(
    State(state): State<AppState>,
    ApiUser(s): ApiUser,
    Path((ns, repo, number)): Path<(String, String, i32)>,
    Json(b): Json<UpdateBranchBody>,
) -> Result<Json<MergeRequest>, AppError> {
    let m = state
        .services
        .update_mr_branch(
            &s,
            (&ns, &repo, number),
            &b.method,
            b.expected_head_oid.as_deref(),
        )
        .await?;
    Ok(Json(render(&state, &s, &ns, &repo, m).await?))
}

/// An entry of the review continuity ledger.
#[derive(Serialize, ToSchema)]
pub struct LedgerEntry {
    /// `binding`, `approval_carried`, `approval_stale`, `status_stale`,
    /// `target_moved`, or `signature_removed`.
    pub kind: String,
    pub revision: Option<i32>,
    /// The change's ID in jj's format, where it's about one change.
    pub change_id: Option<String>,
    /// For bindings: `new`, `confirmed`, `inferred`, or `unresolved`.
    pub state: Option<String>,
    pub reason: Option<String>,
    /// Kind-specific facts (commit, the alias or patch that decided,
    /// candidates, reviewer, status context, old and new heads).
    #[schema(value_type = Object)]
    pub detail: serde_json::Value,
    pub actor: Option<String>,
    pub created_at: String,
}

/// Every identity decision and effect on review state, oldest first.
#[utoipa::path(get, path = "/api/v1/repos/{ns}/{repo}/merge-requests/{number}/ledger", tag = "merge requests", security((), ("token" = ["repo:read"])), params(("ns" = String, Path), ("repo" = String, Path), ("number" = i32, Path)),
    responses((status = 200, body = Vec<LedgerEntry>), (status = 404, body = super::Problem)))]
pub async fn get_ledger(
    State(state): State<AppState>,
    ApiSubject(s): ApiSubject,
    Path((ns, repo, number)): Path<(String, String, i32)>,
) -> Result<Json<Vec<LedgerEntry>>, AppError> {
    let (_, m) = state.services.mr(&s, &ns, &repo, number).await?;
    let entries = state.services.mr_ledger(&m).await?;
    Ok(Json(
        entries
            .into_iter()
            .map(|e| LedgerEntry {
                kind: e.kind,
                revision: e.revision_seq,
                change_id: e.change_id,
                state: e.state,
                reason: e.reason,
                detail: e.detail,
                actor: e.actor,
                created_at: ts(e.created_at),
            })
            .collect(),
    ))
}

pub fn router() -> Router<AppState> {
    use axum::routing::get;
    Router::new()
        .route_as(
            "/api/v1/repos/{ns}/{repo}/merge-requests",
            Access::Resource,
            get(list_mrs).post(create_mr),
        )
        .route_as(
            "/api/v1/repos/{ns}/{repo}/merge-requests/{number}",
            Access::Resource,
            get(get_mr).patch(update_mr),
        )
        .route_as(
            "/api/v1/repos/{ns}/{repo}/merge-requests/{number}/comments",
            Access::Resource,
            get(list_mr_comments).post(create_mr_comment),
        )
        .route_as(
            "/api/v1/repos/{ns}/{repo}/merge-requests/{number}/reviews",
            Access::Resource,
            get(list_reviews).post(create_review),
        )
        .route_as(
            "/api/v1/repos/{ns}/{repo}/merge-requests/{number}/reviewers",
            Access::Resource,
            axum::routing::put(put_reviewers),
        )
        .route_as(
            "/api/v1/repos/{ns}/{repo}/merge-requests/{number}/ledger",
            Access::Resource,
            get(get_ledger),
        )
        .route_as(
            "/api/v1/repos/{ns}/{repo}/merge-requests/{number}/merge",
            Access::Resource,
            axum::routing::post(merge),
        )
        .route_as(
            "/api/v1/repos/{ns}/{repo}/merge-requests/{number}/inventory",
            Access::Resource,
            get(get_inventory),
        )
        .route_as(
            "/api/v1/repos/{ns}/{repo}/merge-requests/{number}/checkout",
            Access::Resource,
            axum::routing::post(checkout),
        )
        .route_as(
            "/api/v1/repos/{ns}/{repo}/merge-requests/{number}/suggestions/apply",
            Access::Resource,
            axum::routing::post(apply_suggestions),
        )
        .route_as(
            "/api/v1/repos/{ns}/{repo}/merge-requests/{number}/auto-merge",
            Access::Resource,
            axum::routing::put(put_auto_merge).delete(delete_auto_merge),
        )
        .route_as(
            "/api/v1/repos/{ns}/{repo}/merge-requests/{number}/update-branch",
            Access::Resource,
            axum::routing::post(update_branch),
        )
        .route_as(
            "/api/v1/repos/{ns}/{repo}/merge-requests/{number}/mergeability",
            Access::Resource,
            get(get_mergeability),
        )
        .route_as(
            "/api/v1/repos/{ns}/{repo}/merge-requests/{number}/stack",
            Access::Resource,
            get(get_stack),
        )
        .route_as(
            "/api/v1/repos/{ns}/{repo}/merge-requests/{number}/stack/{position}/bind",
            Access::Resource,
            axum::routing::post(bind_change),
        )
}
