// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! The code browser: repository home with README, trees, files, raw files,
//! history, commits with diffs, branches, and tags.
//!
//! Paths come from the raw URL and are percent-decoded to bytes, so
//! non-UTF-8 file names work. `/tree/<ref>/<path>` resolves the longest
//! existing branch, then tag, then a full commit ID.

use std::time::Duration;

use askama::Template;
use axum::Router;
use axum::extract::{Query, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Redirect, Response};
use serde::Deserialize;
use slath_core::authz::{Action, Subject, Visibility};
use slath_git::ObjectReader;
use slath_git::browse::{self, PathObject, RefKind, RefPath};
use slath_git::object::{Blob, EntryKind, ObjectId, TreeEntry};
use slath_service::repos::RepoAccess;

use super::repos::{RepoHeader, clone_urls};
use super::{MarkupHtml, PageCtx, Time, render};
use crate::AppState;
use crate::auth::MaybeSession;
use crate::error::AppError;
use crate::routes::{Access, RouteExt};

/// Percent-encodes a path for URLs, keeping `/` and unreserved characters.
#[must_use]
pub fn pct(bytes: &[u8]) -> String {
    let mut out = String::new();
    for &b in bytes {
        if b.is_ascii_alphanumeric() || b"-._~/".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn lossy(b: &[u8]) -> String {
    slath_markup::reveal_bidi(&String::from_utf8_lossy(b)).into_owned()
}

fn time(unix: i64) -> Time {
    time::OffsetDateTime::from_unix_timestamp(unix)
        .unwrap_or(time::OffsetDateTime::UNIX_EPOCH)
        .into()
}

/// `/{ns}/{repo}/{view}/{rest…}` from the raw request URI, with `rest`
/// percent-decoded to bytes. (axum's path parameters must be UTF-8; file
/// and ref names needn't be.)
#[derive(Debug, Clone)]
pub struct RawRepoPath(String, String, Vec<u8>);

impl<S: Send + Sync> axum::extract::FromRequestParts<S> for RawRepoPath {
    type Rejection = AppError;
    async fn from_request_parts(
        parts: &mut axum::http::request::Parts,
        _: &S,
    ) -> Result<Self, Self::Rejection> {
        let path = parts.uri.path();
        let mut segs = path.splitn(5, '/').skip(1);
        let dec = |s: &str| -> Vec<u8> { percent_encoding::percent_decode_str(s).collect() };
        let ns =
            String::from_utf8(dec(segs.next().unwrap_or(""))).map_err(|_| AppError::not_found())?;
        let repo =
            String::from_utf8(dec(segs.next().unwrap_or(""))).map_err(|_| AppError::not_found())?;
        let _view = segs.next();
        let rest = dec(segs.next().unwrap_or(""));
        Ok(Self(ns, repo, rest))
    }
}

fn params(raw: &RawRepoPath) -> (String, String, Vec<u8>) {
    (raw.0.clone(), raw.1.clone(), raw.2.clone())
}

async fn access(
    state: &AppState,
    subject: &Subject,
    ns: &str,
    repo: &str,
) -> Result<RepoAccess, AppError> {
    Ok(state
        .services
        .repo_access(subject, ns, repo, Action::RepoRead)
        .await?)
}

fn git_err(e: &slath_git::GitError) -> AppError {
    match e {
        slath_git::GitError::NotFound | slath_git::GitError::InvalidArgument(_) => {
            AppError::not_found()
        }
        e => AppError::internal(e),
    }
}

async fn resolve(state: &AppState, a: &RepoAccess, rest: &[u8]) -> Result<RefPath, AppError> {
    browse::resolve_ref_path(&state.runner, &state.services.repo_path(a.repo.id), rest)
        .await
        .map_err(|e| git_err(&e))?
        .ok_or_else(AppError::not_found)
}

/// A link target inside the repository: `/{ns}/{repo}/{kind}/{ref}/{path}`.
fn link(a: &RepoAccess, kind: &str, r: &RefPath, path: &[u8]) -> String {
    let mut s = format!(
        "/{}/{}/{kind}/{}",
        a.repo.namespace,
        a.repo.name,
        pct(&r.name)
    );
    if !path.is_empty() {
        s.push('/');
        s.push_str(&pct(path));
    }
    s
}

/// One step of the breadcrumb trail.
struct Crumb {
    name: String,
    href: String,
}

fn crumbs(a: &RepoAccess, r: &RefPath) -> Vec<Crumb> {
    let mut out = vec![Crumb {
        name: a.repo.name.clone(),
        href: link(a, "tree", r, b""),
    }];
    let mut acc: Vec<u8> = Vec::new();
    for part in r
        .path
        .as_bytes()
        .split(|&b| b == b'/')
        .filter(|p| !p.is_empty())
    {
        if !acc.is_empty() {
            acc.push(b'/');
        }
        acc.extend_from_slice(part);
        out.push(Crumb {
            name: lossy(part),
            href: link(a, "tree", r, &acc),
        });
    }
    out
}

struct EntryView {
    name: String,
    href: String,
    kind: &'static str,
    icon: &'static str,
}

fn entry_views(a: &RepoAccess, r: &RefPath, entries: &[TreeEntry]) -> Vec<EntryView> {
    entries
        .iter()
        .map(|e| {
            let mut path = r.path.as_bytes().to_vec();
            if !path.is_empty() {
                path.push(b'/');
            }
            path.extend_from_slice(&e.name);
            let (kind, icon, view) = match e.kind() {
                EntryKind::Tree => ("Directory", "folder", "tree"),
                EntryKind::Symlink => ("Symbolic link", "link", "blob"),
                EntryKind::Submodule => ("Submodule", "git-commit-horizontal", "blob"),
                EntryKind::Executable => ("Executable file", "file-code", "blob"),
                EntryKind::File => ("File", "file", "blob"),
            };
            EntryView {
                name: lossy(&e.name),
                href: link(a, view, r, &path),
                kind,
                icon,
            }
        })
        .collect()
}

/// The ref being browsed, for the header line.
struct RefView {
    kind: &'static str,
    name: String,
    commit: String,
    short: String,
}

fn ref_view(r: &RefPath) -> RefView {
    RefView {
        kind: match r.kind {
            RefKind::Branch => "Branch",
            RefKind::Tag => "Tag",
            RefKind::Commit => "Commit",
        },
        name: lossy(&r.name),
        commit: r.commit.as_str().to_owned(),
        short: r.commit.as_str()[..12].to_owned(),
    }
}

#[derive(Template)]
#[template(path = "repo/tree.html")]
struct TreePage {
    ctx: PageCtx,
    header: RepoHeader,
    title: String,
    reference: RefView,
    crumbs: Vec<Crumb>,
    entries: Vec<EntryView>,
    readme: Option<Readme>,
    commits_href: String,
    clone: Option<(String, String)>,
    description: Option<String>,
}

/// A README rendered below the tree.
struct Readme {
    name: String,
    /// Sanitized HTML (Markdown) or `None` for plain text.
    html: Option<MarkupHtml>,
    text: String,
}

const README_NAMES: [&[u8]; 4] = [b"README.md", b"README", b"README.txt", b"readme.md"];

async fn readme(
    state: &AppState,
    reader: &mut ObjectReader,
    entries: &[TreeEntry],
) -> Option<Readme> {
    let e = README_NAMES.iter().find_map(|n| {
        entries.iter().find(|e| {
            e.name.as_slice() == *n && matches!(e.kind(), EntryKind::File | EntryKind::Executable)
        })
    })?;
    let limit = state.config.limits.render_max_file_bytes.as_usize();
    let Ok(Blob::Data(data)) = reader.read_blob(&e.oid, limit).await else {
        return None;
    };
    if slath_markup::is_binary(&data) {
        return None;
    }
    let text = String::from_utf8_lossy(&data).into_owned();
    let name = lossy(&e.name);
    let html = if name.to_ascii_lowercase().ends_with(".md") {
        let opts = slath_markup::MarkdownOptions {
            external_images: state.config.ui.external_images,
        };
        let src = text.clone();
        let rendered = tokio::time::timeout(
            Duration::from_secs(2),
            tokio::task::spawn_blocking(move || slath_markup::render_markdown(&src, opts)),
        )
        .await;
        match rendered {
            Ok(Ok(h)) => Some(MarkupHtml::from_markup(h)),
            _ => None,
        }
    } else {
        None
    };
    Some(Readme { name, html, text })
}

async fn tree_page(
    state: &AppState,
    ctx: PageCtx,
    subject: &Subject,
    a: &RepoAccess,
    r: RefPath,
    home: bool,
) -> Result<Response, AppError> {
    let mut reader = ObjectReader::open(&state.runner, &state.services.repo_path(a.repo.id))
        .await
        .map_err(|e| AppError::internal(&e))?;
    let entries = match browse::lookup_path(&mut reader, &r.commit, &r.path)
        .await
        .map_err(|e| git_err(&e))?
    {
        Some(PathObject::Tree(entries)) => entries,
        Some(PathObject::Blob { .. }) => {
            return Ok(Redirect::to(&link(a, "blob", &r, r.path.as_bytes())).into_response());
        }
        None => return Err(AppError::not_found()),
    };
    let readme = if r.path.is_root() {
        readme(state, &mut reader, &entries).await
    } else {
        None
    };
    let title = if r.path.is_root() {
        format!("{}/{}", a.repo.namespace, a.repo.name)
    } else {
        format!(
            "{} · {}/{}",
            lossy(r.path.as_bytes()),
            a.repo.namespace,
            a.repo.name
        )
    };
    Ok(render(&TreePage {
        header: RepoHeader::new(a, subject, "code"),
        title,
        reference: ref_view(&r),
        crumbs: crumbs(a, &r),
        entries: entry_views(a, &r, &entries),
        readme,
        commits_href: link(a, "commits", &r, r.path.as_bytes()),
        clone: home.then(|| clone_urls(state, &a.repo.namespace, &a.repo.name)),
        description: if home {
            a.repo.description.clone()
        } else {
            None
        },
        ctx,
    }))
}

/// The repository home of a repository with content: the default branch's
/// root tree and README.
pub async fn repo_home_with_content(
    state: AppState,
    ctx: PageCtx,
    subject: Subject,
    a: RepoAccess,
) -> Result<Response, AppError> {
    let git_dir = state.services.repo_path(a.repo.id);
    let mut r = browse::resolve_ref_path(&state.runner, &git_dir, a.repo.default_branch.as_bytes())
        .await
        .map_err(|e| git_err(&e))?;
    if r.is_none() {
        // The default branch doesn't exist (yet): show the newest branch.
        if let Some(b) = browse::list_refs(&state.runner, &git_dir, RefKind::Branch, 1)
            .await
            .map_err(|e| git_err(&e))?
            .first()
        {
            r = browse::resolve_ref_path(&state.runner, &git_dir, &b.name)
                .await
                .map_err(|e| git_err(&e))?;
        }
    }
    let Some(r) = r else {
        return Err(AppError::not_found());
    };
    tree_page(&state, ctx, &subject, &a, r, true).await
}

async fn tree(
    State(state): State<AppState>,
    ctx: PageCtx,
    session: MaybeSession,
    raw: RawRepoPath,
) -> Result<Response, AppError> {
    let subject = session.subject();
    let (ns, repo, rest) = params(&raw);
    let a = access(&state, &subject, &ns, &repo).await?;
    let r = resolve(&state, &a, &rest).await?;
    tree_page(&state, ctx, &subject, &a, r, false).await
}

/// One line of a file view.
struct CodeLine {
    no: usize,
    html: MarkupHtml,
}

#[derive(Template)]
#[template(path = "repo/blob.html")]
struct BlobPage {
    ctx: PageCtx,
    header: RepoHeader,
    title: String,
    reference: RefView,
    crumbs: Vec<Crumb>,
    raw_href: String,
    commits_href: String,
    permalink: String,
    /// Why the content isn't shown, if it isn't.
    notice: Option<String>,
    /// Symlink target or submodule commit.
    special: Option<String>,
    lines: Vec<CodeLine>,
    language: String,
    size: u64,
}

async fn blob(
    State(state): State<AppState>,
    ctx: PageCtx,
    session: MaybeSession,
    raw: RawRepoPath,
) -> Result<Response, AppError> {
    let subject = session.subject();
    let (ns, repo, rest) = params(&raw);
    let a = access(&state, &subject, &ns, &repo).await?;
    let r = resolve(&state, &a, &rest).await?;
    let mut reader = ObjectReader::open(&state.runner, &state.services.repo_path(a.repo.id))
        .await
        .map_err(|e| AppError::internal(&e))?;
    let entry = match browse::lookup_path(&mut reader, &r.commit, &r.path)
        .await
        .map_err(|e| git_err(&e))?
    {
        Some(PathObject::Blob { entry }) => entry,
        Some(PathObject::Tree(_)) => {
            return Ok(Redirect::to(&link(&a, "tree", &r, r.path.as_bytes())).into_response());
        }
        None => return Err(AppError::not_found()),
    };
    let limit = state.config.limits.render_max_file_bytes.as_usize();
    let path_str = String::from_utf8_lossy(r.path.as_bytes()).into_owned();
    let mut page = BlobPage {
        header: RepoHeader::new(&a, &subject, "code"),
        title: format!(
            "{} · {}/{}",
            lossy(r.path.as_bytes()),
            a.repo.namespace,
            a.repo.name
        ),
        reference: ref_view(&r),
        crumbs: crumbs(&a, &r),
        raw_href: link(&a, "raw", &r, r.path.as_bytes()),
        commits_href: link(&a, "commits", &r, r.path.as_bytes()),
        permalink: format!(
            "/{}/{}/blob/{}/{}",
            a.repo.namespace,
            a.repo.name,
            r.commit.as_str(),
            pct(r.path.as_bytes())
        ),
        notice: None,
        special: None,
        lines: Vec::new(),
        language: String::new(),
        size: 0,
        ctx,
    };
    match entry.kind() {
        EntryKind::Symlink => {
            if let Ok(Blob::Data(target)) = reader.read_blob(&entry.oid, 4096).await {
                page.special = Some(format!("Symbolic link to {}", lossy(&target)));
            }
        }
        EntryKind::Submodule => {
            page.special = Some(format!("Submodule at commit {}", entry.oid.as_str()));
        }
        EntryKind::Tree => return Err(AppError::not_found()),
        EntryKind::File | EntryKind::Executable => match reader
            .read_blob(&entry.oid, limit)
            .await
            .map_err(|e| git_err(&e))?
        {
            Blob::TooLarge { size } => {
                page.size = size;
                page.notice = Some(format!("This file is too large to display ({size} bytes)."));
            }
            Blob::Data(data) => {
                page.size = data.len() as u64;
                if slath_markup::is_binary(&data) {
                    page.notice = Some("This is a binary file.".to_owned());
                } else {
                    let text = String::from_utf8_lossy(&data).into_owned();
                    let p = path_str.clone();
                    let t2 = text.clone();
                    let h = match tokio::time::timeout(
                        Duration::from_secs(2),
                        tokio::task::spawn_blocking(move || slath_markup::highlight(&p, &t2)),
                    )
                    .await
                    {
                        Ok(Ok(h)) => h,
                        _ => slath_markup::highlight("", &text),
                    };
                    page.language = h.language;
                    page.lines = h
                        .lines
                        .into_iter()
                        .enumerate()
                        .map(|(i, html)| CodeLine {
                            no: i + 1,
                            html: MarkupHtml::from_markup(html),
                        })
                        .collect();
                }
            }
        },
    }
    Ok(render(&page))
}

/// Content type for raw files: text, an allowlisted image (by its bytes),
/// or a download. HTML, SVG, XML, and PDF are always downloads.
fn raw_type(data: &[u8]) -> (&'static str, bool) {
    if data.starts_with(b"\x89PNG\r\n\x1a\n") {
        ("image/png", false)
    } else if data.starts_with(&[0xFF, 0xD8, 0xFF]) {
        ("image/jpeg", false)
    } else if data.starts_with(b"GIF87a") || data.starts_with(b"GIF89a") {
        ("image/gif", false)
    } else if data.len() > 12 && &data[..4] == b"RIFF" && &data[8..12] == b"WEBP" {
        ("image/webp", false)
    } else if !slath_markup::is_binary(data) && std::str::from_utf8(data).is_ok() {
        let head = String::from_utf8_lossy(&data[..data.len().min(1024)]).to_ascii_lowercase();
        let markup = [
            "<!doctype",
            "<html",
            "<svg",
            "<?xml",
            "<script",
            "<body",
            "<head",
        ]
        .iter()
        .any(|m| head.trim_start().starts_with(m) || head.contains(m));
        if markup {
            ("application/octet-stream", true)
        } else {
            ("text/plain; charset=utf-8", false)
        }
    } else {
        ("application/octet-stream", true)
    }
}

/// A file name safe for `Content-Disposition`.
fn download_name(path: &[u8]) -> String {
    let name = path.rsplit(|&b| b == b'/').next().unwrap_or(path);
    let s: String = String::from_utf8_lossy(name)
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || "._-".contains(*c))
        .take(100)
        .collect();
    if s.is_empty() {
        "download".to_owned()
    } else {
        s
    }
}

async fn raw(
    State(state): State<AppState>,
    session: MaybeSession,
    raw: RawRepoPath,
) -> Result<Response, AppError> {
    let subject = session.subject();
    let (ns, repo, rest) = params(&raw);
    let a = access(&state, &subject, &ns, &repo).await?;
    let r = resolve(&state, &a, &rest).await?;
    let mut reader = ObjectReader::open(&state.runner, &state.services.repo_path(a.repo.id))
        .await
        .map_err(|e| AppError::internal(&e))?;
    let entry = match browse::lookup_path(&mut reader, &r.commit, &r.path)
        .await
        .map_err(|e| git_err(&e))?
    {
        Some(PathObject::Blob { entry }) if entry.kind() != EntryKind::Submodule => entry,
        _ => return Err(AppError::not_found()),
    };
    let max = state.runner.config().max_output_bytes;
    let data = match reader
        .read_blob(&entry.oid, max)
        .await
        .map_err(|e| git_err(&e))?
    {
        Blob::Data(d) => d,
        Blob::TooLarge { .. } => {
            return Err(AppError::with_code(
                StatusCode::PAYLOAD_TOO_LARGE,
                "too_large",
                "File too large",
            ));
        }
    };
    // A symlink's raw content is its target text; it's never followed.
    let (ct, attachment) = if entry.kind() == EntryKind::Symlink {
        ("text/plain; charset=utf-8", false)
    } else {
        raw_type(&data)
    };
    let mut res = (StatusCode::OK, data).into_response();
    let h = res.headers_mut();
    h.insert(header::CONTENT_TYPE, HeaderValue::from_static(ct));
    h.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static("sandbox; default-src 'none'; style-src 'unsafe-inline'"),
    );
    h.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    if attachment
        && let Ok(v) = HeaderValue::from_str(&format!(
            "attachment; filename=\"{}\"",
            download_name(r.path.as_bytes())
        ))
    {
        h.insert(header::CONTENT_DISPOSITION, v);
    }
    let cache = if a.repo.visibility == Visibility::Private {
        "private, no-store"
    } else {
        "no-cache"
    };
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static(cache));
    Ok(res)
}

struct CommitRow {
    oid: String,
    short: String,
    subject: String,
    author: String,
    time: Time,
}

#[derive(Template)]
#[template(path = "repo/commits.html")]
struct CommitsPage {
    ctx: PageCtx,
    header: RepoHeader,
    reference: RefView,
    path: String,
    commits: Vec<CommitRow>,
    page: usize,
    prev_href: Option<String>,
    next_href: Option<String>,
}

#[derive(Deserialize, Default)]
struct PageQuery {
    page: Option<usize>,
}

const PER_PAGE: usize = 50;
const MAX_PAGE: usize = 2000;

async fn commits_for(
    state: &AppState,
    ctx: PageCtx,
    subject: &Subject,
    a: &RepoAccess,
    r: RefPath,
    page: usize,
) -> Result<Response, AppError> {
    let page = page.clamp(1, MAX_PAGE);
    let git_dir = state.services.repo_path(a.repo.id);
    let mut entries = browse::log(
        &state.runner,
        &git_dir,
        &r.commit,
        &r.path,
        (page - 1) * PER_PAGE,
        PER_PAGE + 1,
    )
    .await
    .map_err(|e| git_err(&e))?;
    let more = entries.len() > PER_PAGE;
    entries.truncate(PER_PAGE);
    let base = link(a, "commits", &r, r.path.as_bytes());
    Ok(render(&CommitsPage {
        header: RepoHeader::new(a, subject, "commits"),
        reference: ref_view(&r),
        path: lossy(r.path.as_bytes()),
        commits: entries
            .into_iter()
            .map(|e| CommitRow {
                short: e.oid.as_str()[..12].to_owned(),
                oid: e.oid.as_str().to_owned(),
                subject: slath_markup::reveal_bidi(&e.subject).into_owned(),
                author: e.author_name,
                time: time(e.author_time),
            })
            .collect(),
        page,
        prev_href: (page > 1).then(|| format!("{base}?page={}", page - 1)),
        next_href: (more && page < MAX_PAGE).then(|| format!("{base}?page={}", page + 1)),
        ctx,
    }))
}

async fn commits(
    State(state): State<AppState>,
    ctx: PageCtx,
    session: MaybeSession,
    raw: RawRepoPath,
    Query(q): Query<PageQuery>,
) -> Result<Response, AppError> {
    let subject = session.subject();
    let (ns, repo, rest) = params(&raw);
    let a = access(&state, &subject, &ns, &repo).await?;
    let rest = if rest.is_empty() {
        a.repo.default_branch.as_bytes().to_vec()
    } else {
        rest
    };
    let r = resolve(&state, &a, &rest).await?;
    commits_for(&state, ctx, &subject, &a, r, q.page.unwrap_or(1)).await
}

struct DiffLineView {
    kind: &'static str,
    old_no: String,
    new_no: String,
    text: String,
}

struct DiffFileView {
    anchor: String,
    path: String,
    old_path: Option<String>,
    status: &'static str,
    binary: bool,
    additions: u32,
    deletions: u32,
    hunks: Vec<(String, Vec<DiffLineView>)>,
}

#[derive(Template)]
#[template(path = "repo/commit.html")]
struct CommitPage {
    ctx: PageCtx,
    header: RepoHeader,
    oid: String,
    short: String,
    subject: String,
    body: String,
    author: String,
    author_time: Time,
    committer: String,
    parents: Vec<(String, String)>,
    files: Vec<DiffFileView>,
    truncated: bool,
    tree_href: String,
}

/// Name and time from a raw `author`/`committer` header value.
fn ident(value: &[u8]) -> (String, i64) {
    let s = String::from_utf8_lossy(value);
    let name = s.split(" <").next().unwrap_or("").to_owned();
    let t = s
        .rsplit('>')
        .next()
        .and_then(|r| r.split_whitespace().next())
        .and_then(|t| t.parse().ok())
        .unwrap_or(0);
    (name, t)
}

async fn commit(
    State(state): State<AppState>,
    ctx: PageCtx,
    session: MaybeSession,
    raw: RawRepoPath,
) -> Result<Response, AppError> {
    let subject = session.subject();
    let (ns, repo, rest) = params(&raw);
    let a = access(&state, &subject, &ns, &repo).await?;
    let oid = ObjectId::parse(&rest).map_err(|_| AppError::not_found())?;
    let git_dir = state.services.repo_path(a.repo.id);
    let oid = browse::resolve_commit(&state.runner, &git_dir, oid.as_str().as_bytes())
        .await
        .map_err(|e| git_err(&e))?
        .ok_or_else(AppError::not_found)?;
    let mut reader = ObjectReader::open(&state.runner, &git_dir)
        .await
        .map_err(|e| AppError::internal(&e))?;
    let c = reader
        .read_raw_commit(&oid)
        .await
        .map_err(|e| git_err(&e))?;
    let parents: Vec<ObjectId> = c
        .parents()
        .filter_map(|p| ObjectId::parse(p).ok())
        .collect();
    let limits = &state.config.limits;
    let (out, too_big) = browse::commit_diff(
        &state.runner,
        &git_dir,
        parents.first(),
        &oid,
        64 * 1024 * 1024,
    )
    .await
    .map_err(|e| git_err(&e))?;
    let diff = slath_git::diff::parse(&out, limits.diff_max_files, limits.diff_max_lines);
    let message = String::from_utf8_lossy(&c.message).into_owned();
    let (subject_line, body) = message
        .split_once('\n')
        .map_or((message.as_str(), ""), |(s, b)| (s, b));
    let (author, author_t) = ident(c.header(b"author").unwrap_or_default());
    let (committer, _) = ident(c.header(b"committer").unwrap_or_default());
    let files = diff
        .files
        .iter()
        .enumerate()
        .map(|(i, f)| DiffFileView {
            anchor: format!("diff-{i}"),
            path: lossy(f.path()),
            old_path: (f.status == slath_git::diff::Status::Renamed
                || f.status == slath_git::diff::Status::Copied)
                .then(|| lossy(&f.old_path)),
            status: match f.status {
                slath_git::diff::Status::Added => "Added",
                slath_git::diff::Status::Deleted => "Deleted",
                slath_git::diff::Status::Modified => "Modified",
                slath_git::diff::Status::Renamed => "Renamed",
                slath_git::diff::Status::Copied => "Copied",
            },
            binary: f.binary,
            additions: f.additions,
            deletions: f.deletions,
            hunks: f
                .hunks
                .iter()
                .map(|h| {
                    let head = format!(
                        "@@ -{} +{} @@ {}",
                        h.old_start,
                        h.new_start,
                        lossy(&h.section)
                    );
                    let lines = h
                        .lines
                        .iter()
                        .map(|l| {
                            use slath_git::diff::LineKind as K;
                            let kind = match l.kind {
                                K::Added => "add",
                                K::Removed => "del",
                                K::Context => "ctx",
                                K::NoNewline => "meta",
                            };
                            DiffLineView {
                                kind,
                                old_no: l.old_no.map(|n| n.to_string()).unwrap_or_default(),
                                new_no: l.new_no.map(|n| n.to_string()).unwrap_or_default(),
                                text: lossy(&l.text),
                            }
                        })
                        .collect();
                    (head, lines)
                })
                .collect(),
        })
        .collect();
    Ok(render(&CommitPage {
        header: RepoHeader::new(&a, &subject, "commits"),
        short: oid.as_str()[..12].to_owned(),
        subject: slath_markup::reveal_bidi(subject_line).into_owned(),
        body: slath_markup::reveal_bidi(body.trim()).into_owned(),
        author,
        author_time: time(author_t),
        committer,
        parents: parents
            .iter()
            .map(|p| (p.as_str().to_owned(), p.as_str()[..12].to_owned()))
            .collect(),
        files,
        truncated: diff.truncated || too_big,
        tree_href: format!(
            "/{}/{}/tree/{}",
            a.repo.namespace,
            a.repo.name,
            oid.as_str()
        ),
        oid: oid.as_str().to_owned(),
        ctx,
    }))
}

struct RefRow {
    name: String,
    href: String,
    commit: String,
    short: String,
    subject: String,
    time: Time,
    default: bool,
}

#[derive(Template)]
#[template(path = "repo/refs.html")]
struct RefsPage {
    ctx: PageCtx,
    header: RepoHeader,
    heading: &'static str,
    refs: Vec<RefRow>,
}

async fn refs(
    state: AppState,
    ctx: PageCtx,
    session: MaybeSession,
    raw: RawRepoPath,
    kind: RefKind,
) -> Result<Response, AppError> {
    let subject = session.subject();
    let (ns, repo, _) = params(&raw);
    let a = access(&state, &subject, &ns, &repo).await?;
    let list = browse::list_refs(
        &state.runner,
        &state.services.repo_path(a.repo.id),
        kind,
        500,
    )
    .await
    .map_err(|e| git_err(&e))?;
    let (tab, heading) = if kind == RefKind::Branch {
        ("branches", "Branches")
    } else {
        ("tags", "Tags")
    };
    Ok(render(&RefsPage {
        header: RepoHeader::new(&a, &subject, tab),
        heading,
        refs: list
            .into_iter()
            .map(|r| RefRow {
                default: kind == RefKind::Branch && r.name == a.repo.default_branch.as_bytes(),
                href: format!(
                    "/{}/{}/tree/{}",
                    a.repo.namespace,
                    a.repo.name,
                    pct(&r.name)
                ),
                name: lossy(&r.name),
                short: r.commit.as_str()[..12].to_owned(),
                commit: r.commit.as_str().to_owned(),
                subject: slath_markup::reveal_bidi(&r.subject).into_owned(),
                time: time(r.time),
            })
            .collect(),
        ctx,
    }))
}

/// `GET /{ns}/{repo}/archive/{ref}.tar.gz|.zip`: a streamed `git archive`
/// of the ref, bounded by `limits.archive_timeout`, a size cap, and
/// `limits.archive_max_concurrent`.
async fn archive(
    State(state): State<AppState>,
    session: MaybeSession,
    raw: RawRepoPath,
) -> Result<Response, AppError> {
    use futures_util::StreamExt;
    let subject = session.subject();
    let (ns, repo, rest) = params(&raw);
    let (name, format, ext, ct) = if let Some(n) = rest.strip_suffix(b".tar.gz") {
        (n, "tar.gz", "tar.gz", "application/gzip")
    } else if let Some(n) = rest.strip_suffix(b".zip") {
        (n, "zip", "zip", "application/zip")
    } else {
        return Err(AppError::not_found());
    };
    let a = access(&state, &subject, &ns, &repo).await?;
    let r = resolve(&state, &a, name).await?;
    if !r.path.is_root() {
        return Err(AppError::not_found());
    }
    let Ok(permit) = std::sync::Arc::clone(&state.archives).try_acquire_owned() else {
        return Err(AppError::with_code(
            StatusCode::SERVICE_UNAVAILABLE,
            "busy",
            "Too many downloads",
        )
        .detail("Too many archives are being prepared. Try again shortly."));
    };
    let short = &r.commit.as_str()[..12];
    let prefix = format!("{}-{short}/", a.repo.name);
    let mut child = state
        .runner
        .repo(&state.services.repo_path(a.repo.id))
        .args([
            "archive",
            &format!("--format={format}"),
            &format!("--prefix={prefix}"),
            "--end-of-options",
            r.commit.as_str(),
        ])
        .spawn()
        .await
        .map_err(|e| AppError::internal(&e))?;
    let (stdin, stdout, stderr) = child.take_stdio().map_err(|e| AppError::internal(&e))?;
    drop(stdin);
    tokio::spawn(async move {
        let mut e = stderr;
        let mut sink = Vec::new();
        let _ = tokio::io::AsyncReadExt::read_to_end(&mut e, &mut sink).await;
    });
    let limits = &state.config.limits;
    let deadline = tokio::time::Instant::now() + limits.archive_timeout.get();
    let cap = limits.push_max_bytes.get();
    let out = tokio_util::io::ReaderStream::new(stdout);
    // The stream owns the child and the permit; dropping it kills Git.
    let stream = futures_util::stream::unfold(
        (out, Some((child, permit)), 0u64),
        move |(mut out, mut guard, sent)| async move {
            guard.as_ref()?;
            match tokio::time::timeout_at(deadline, out.next()).await {
                Ok(Some(Ok(chunk))) if sent + chunk.len() as u64 <= cap => {
                    let sent = sent + chunk.len() as u64;
                    Some((Ok(chunk), (out, guard, sent)))
                }
                Ok(None) => {
                    if let Some((mut c, _p)) = guard.take() {
                        let _ = c.wait().await;
                    }
                    None
                }
                Ok(Some(Err(e))) => {
                    drop(guard.take());
                    Some((Err(e), (out, None, sent)))
                }
                _ => {
                    drop(guard.take());
                    Some((
                        Err(std::io::Error::other("archive timed out or grew too large")),
                        (out, None, sent),
                    ))
                }
            }
        },
    );
    let mut res = axum::body::Body::from_stream(stream).into_response();
    let h = res.headers_mut();
    h.insert(header::CONTENT_TYPE, HeaderValue::from_static(ct));
    if let Ok(v) = HeaderValue::from_str(&format!(
        "attachment; filename=\"{}-{short}.{ext}\"",
        download_name(a.repo.name.as_bytes())
    )) {
        h.insert(header::CONTENT_DISPOSITION, v);
    }
    h.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static("sandbox; default-src 'none'"),
    );
    let cache = if a.repo.visibility == Visibility::Private {
        "private, no-store"
    } else {
        "no-cache"
    };
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static(cache));
    Ok(res)
}

pub fn router() -> Router<AppState> {
    use axum::routing::get;
    Router::new()
        .route_as("/{ns}/{repo}/tree/{*rest}", Access::Resource, get(tree))
        .route_as("/{ns}/{repo}/blob/{*rest}", Access::Resource, get(blob))
        .route_as("/{ns}/{repo}/raw/{*rest}", Access::Resource, get(raw))
        .route_as("/{ns}/{repo}/commits", Access::Resource, get(commits))
        .route_as(
            "/{ns}/{repo}/commits/{*rest}",
            Access::Resource,
            get(commits),
        )
        .route_as("/{ns}/{repo}/commit/{rest}", Access::Resource, get(commit))
        .route_as(
            "/{ns}/{repo}/archive/{*rest}",
            Access::Resource,
            get(archive),
        )
        .route_as(
            "/{ns}/{repo}/branches",
            Access::Resource,
            get(
                |s: State<AppState>, c: PageCtx, m: MaybeSession, r: RawRepoPath| {
                    refs(s.0, c, m, r, RefKind::Branch)
                },
            ),
        )
        .route_as(
            "/{ns}/{repo}/tags",
            Access::Resource,
            get(
                |s: State<AppState>, c: PageCtx, m: MaybeSession, r: RawRepoPath| {
                    refs(s.0, c, m, r, RefKind::Tag)
                },
            ),
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raw_types() {
        assert_eq!(raw_type(b"hello\n"), ("text/plain; charset=utf-8", false));
        assert_eq!(
            raw_type(b"<svg onload=alert(1)></svg>"),
            ("application/octet-stream", true)
        );
        assert_eq!(
            raw_type(b"  <!DOCTYPE html><html>"),
            ("application/octet-stream", true)
        );
        assert_eq!(
            raw_type(b"<?xml version=\"1.0\"?>"),
            ("application/octet-stream", true)
        );
        assert_eq!(raw_type(b"\x89PNG\r\n\x1a\nrest"), ("image/png", false));
        assert_eq!(
            raw_type(b"\x00\x01binary"),
            ("application/octet-stream", true)
        );
        assert_eq!(download_name(b"dir/x\"; y.svg"), "x_y.svg".replace('_', ""));
    }

    #[test]
    fn percent_encoding_keeps_slashes() {
        assert_eq!(pct(b"a b/c%d/\xff"), "a%20b/c%25d/%FF");
    }
}
