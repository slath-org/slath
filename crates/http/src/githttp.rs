// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Git smart HTTP without CGI.
//!
//! Authentication is HTTP Basic with a personal access token as the
//! password (any non-empty username); account passwords never work here.
//! Without credentials, public repositories can be fetched. Missing or wrong
//! credentials give the same 401 for private and non-existent repositories;
//! an authenticated user without access gets 404.
//!
//! `info/refs` buffers the (capped) advertisement. The RPC endpoints stream:
//! the request body (gzip-decoded if needed) is piped into Git's stdin while
//! Git's stdout streams back as the response, with an idle timeout and a
//! byte limit. If the client goes away, the response stream is dropped,
//! which kills the Git process group.

use std::io;
use std::time::Duration;

use async_compression::tokio::bufread::GzipDecoder;
use axum::Router;
use axum::body::{Body, Bytes};
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use base64::Engine;
use futures_util::{StreamExt, TryStreamExt};
use serde::Deserialize;
use slath_core::authz::Subject;
use slath_git::transport::{self, HookEnv, Service};
use slath_service::ServiceError;
use slath_service::push::{PushGrant, Transport};
use slath_service::repos::{RepoAccess, Resolved};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio_util::io::{ReaderStream, StreamReader};

use crate::AppState;
use crate::routes::{Access, RouteExt};

pub fn router() -> Router<AppState> {
    Router::new()
        .route_as("/{ns}/{repo}/info/refs", Access::Git, get(info_refs))
        .route_as(
            "/{ns}/{repo}/git-upload-pack",
            Access::Git,
            post(upload_pack),
        )
        .route_as(
            "/{ns}/{repo}/git-receive-pack",
            Access::Git,
            post(receive_pack),
        )
        // The same repositories under a path that always asks for
        // credentials: Git sends them over HTTPS only when asked, and
        // fetches of public repositories would otherwise stay anonymous
        // (and miss the fork merge request branches meant for the user).
        .route_as(
            "/-/auth/{ns}/{repo}/info/refs",
            Access::Git,
            get(info_refs_auth),
        )
        .route_as(
            "/-/auth/{ns}/{repo}/git-upload-pack",
            Access::Git,
            post(upload_pack_auth),
        )
        .route_as(
            "/-/auth/{ns}/{repo}/git-receive-pack",
            Access::Git,
            post(receive_pack_auth),
        )
}

fn needs_credentials(headers: &HeaderMap) -> bool {
    !headers.contains_key(header::AUTHORIZATION)
}

async fn info_refs_auth(
    state: State<AppState>,
    path: Path<(String, String)>,
    query: Query<InfoRefsQuery>,
    headers: HeaderMap,
) -> Response {
    if needs_credentials(&headers) {
        return Denied::Unauthorized.into_response();
    }
    info_refs(state, path, query, headers).await
}

async fn upload_pack_auth(
    state: State<AppState>,
    path: Path<(String, String)>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    if needs_credentials(&headers) {
        return Denied::Unauthorized.into_response();
    }
    upload_pack(state, path, headers, body).await
}

async fn receive_pack_auth(
    state: State<AppState>,
    path: Path<(String, String)>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    if needs_credentials(&headers) {
        return Denied::Unauthorized.into_response();
    }
    receive_pack(state, path, headers, body).await
}

#[derive(Deserialize)]
struct InfoRefsQuery {
    service: Option<String>,
}

/// Why a request was refused.
#[derive(Debug, Clone, Copy)]
enum Denied {
    /// Missing or wrong credentials (identical for every repository).
    Unauthorized,
    /// Credentials that are an account password instead of a token.
    PasswordRejected,
    NotFound,
    /// The user can read the repository but not push to it.
    Forbidden,
    Internal,
}

impl IntoResponse for Denied {
    fn into_response(self) -> Response {
        match self {
            Self::Unauthorized => unauthorized("authentication required\n"),
            Self::PasswordRejected => unauthorized(
                "account passwords don't work for Git over HTTPS; use a personal access token (Settings > Access tokens)\n",
            ),
            Self::NotFound => (StatusCode::NOT_FOUND, "repository not found\n").into_response(),
            Self::Forbidden => (
                StatusCode::FORBIDDEN,
                "you don't have permission to push to this repository\n",
            )
                .into_response(),
            Self::Internal => {
                (StatusCode::INTERNAL_SERVER_ERROR, "internal error\n").into_response()
            }
        }
    }
}

fn unauthorized(body: &'static str) -> Response {
    (
        StatusCode::UNAUTHORIZED,
        [(header::WWW_AUTHENTICATE, "Basic realm=\"Slath\"")],
        body,
    )
        .into_response()
}

/// The subject of a Git HTTP request: anonymous without `Authorization`,
/// otherwise the user of a valid token given as the Basic password.
async fn subject(state: &AppState, headers: &HeaderMap) -> Result<Subject, Denied> {
    let Some(value) = headers.get(header::AUTHORIZATION) else {
        return Ok(Subject::Anonymous);
    };
    let encoded = value
        .to_str()
        .ok()
        .and_then(|v| v.strip_prefix("Basic "))
        .ok_or(Denied::Unauthorized)?;
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(encoded.trim())
        .map_err(|_| Denied::Unauthorized)?;
    let decoded = String::from_utf8(decoded).map_err(|_| Denied::Unauthorized)?;
    let (user, password) = decoded.split_once(':').ok_or(Denied::Unauthorized)?;
    if user.is_empty() {
        return Err(Denied::Unauthorized);
    }
    if !slath_auth::token::is_pat(password) {
        return Err(Denied::PasswordRejected);
    }
    match state.services.authenticate_token(password).await {
        Ok(Some(auth)) => Ok(auth.subject),
        Ok(None) => Err(Denied::Unauthorized),
        Err(e) => {
            tracing::error!(error = %e, "token lookup failed");
            Err(Denied::Internal)
        }
    }
}

async fn authorize(
    state: &AppState,
    subject: &Subject,
    ns: &str,
    repo: &str,
    write: bool,
) -> Result<RepoAccess, Denied> {
    let anonymous = matches!(subject, Subject::Anonymous);
    match state
        .services
        .transport_access(subject, ns, repo, write)
        .await
    {
        Ok(a) => Ok(a),
        // Ask anonymous clients for credentials, whether or not the
        // repository exists; never tell them which.
        Err(ServiceError::NotFound | ServiceError::Forbidden) if anonymous => {
            Err(Denied::Unauthorized)
        }
        Err(ServiceError::NotFound) => Err(Denied::NotFound),
        Err(ServiceError::Forbidden) => Err(Denied::Forbidden),
        Err(e) => {
            tracing::error!(error = %e, "git transport authorization failed");
            Err(Denied::Internal)
        }
    }
}

fn internal(e: &dyn std::fmt::Display) -> Response {
    tracing::error!(error = %e, "git transport failed");
    Denied::Internal.into_response()
}

fn git_protocol(headers: &HeaderMap) -> Option<&str> {
    headers.get("git-protocol").and_then(|v| v.to_str().ok())
}

fn no_cache(content_type: &'static str) -> [(header::HeaderName, HeaderValue); 3] {
    [
        (header::CONTENT_TYPE, HeaderValue::from_static(content_type)),
        (
            header::CACHE_CONTROL,
            HeaderValue::from_static("no-cache, max-age=0, must-revalidate"),
        ),
        (header::PRAGMA, HeaderValue::from_static("no-cache")),
    ]
}

async fn info_refs(
    State(state): State<AppState>,
    Path((ns, repo)): Path<(String, String)>,
    Query(q): Query<InfoRefsQuery>,
    headers: HeaderMap,
) -> Response {
    // Dumb HTTP (no `service`) isn't supported.
    let Some(service) = q.service.as_deref().and_then(Service::from_name) else {
        return Denied::NotFound.into_response();
    };
    let subject = match subject(&state, &headers).await {
        Ok(s) => s,
        Err(d) => return d.into_response(),
    };
    // A renamed repository redirects readers, so clients learn the new URL.
    if let Ok(Resolved::Moved { namespace, name }) =
        state.services.resolve_repo(&subject, &ns, &repo).await
    {
        let suffix = if repo.ends_with(".git") { ".git" } else { "" };
        let loc = format!(
            "/{namespace}/{name}{suffix}/info/refs?service={}",
            service.name()
        );
        return (StatusCode::MOVED_PERMANENTLY, [(header::LOCATION, loc)]).into_response();
    }
    let access = match authorize(&state, &subject, &ns, &repo, service.is_write()).await {
        Ok(a) => a,
        Err(d) => return d.into_response(),
    };
    let git_dir = state.services.repo_path(access.repo.id);
    let shown = state
        .services
        .shown_mr_branches(&subject, access.repo.id, service.is_write())
        .await;
    match transport::http_info_refs(
        &state.runner,
        &git_dir,
        service,
        git_protocol(&headers),
        &shown,
    )
    .await
    {
        Ok(body) => {
            let ct = match service {
                Service::UploadPack => "application/x-git-upload-pack-advertisement",
                Service::ReceivePack => "application/x-git-receive-pack-advertisement",
            };
            (no_cache(ct), body).into_response()
        }
        Err(e) => internal(&e),
    }
}

async fn upload_pack(
    State(state): State<AppState>,
    Path((ns, repo)): Path<(String, String)>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    rpc(state, Service::UploadPack, &ns, &repo, &headers, body).await
}

async fn receive_pack(
    State(state): State<AppState>,
    Path((ns, repo)): Path<(String, String)>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    rpc(state, Service::ReceivePack, &ns, &repo, &headers, body).await
}

/// Copies `input` into `output`, failing on an idle period longer than
/// `idle` or more than `limit` bytes.
async fn copy_bounded(
    mut input: impl AsyncRead + Unpin,
    mut output: impl tokio::io::AsyncWrite + Unpin,
    idle: Duration,
    limit: u64,
) -> io::Result<u64> {
    let mut buf = vec![0u8; 64 * 1024];
    let mut total = 0u64;
    loop {
        let n = tokio::time::timeout(idle, input.read(&mut buf))
            .await
            .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "request body idle timeout"))??;
        if n == 0 {
            return Ok(total);
        }
        total += n as u64;
        if total > limit {
            return Err(io::Error::other("request body too large"));
        }
        output.write_all(&buf[..n]).await?;
    }
}

async fn rpc(
    state: AppState,
    service: Service,
    ns: &str,
    repo: &str,
    headers: &HeaderMap,
    body: Body,
) -> Response {
    let subject = match subject(&state, headers).await {
        Ok(s) => s,
        Err(d) => return d.into_response(),
    };
    let access = match authorize(&state, &subject, ns, repo, service.is_write()).await {
        Ok(a) => a,
        Err(d) => return d.into_response(),
    };
    let expected = match service {
        Service::UploadPack => "application/x-git-upload-pack-request",
        Service::ReceivePack => "application/x-git-receive-pack-request",
    };
    if headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        != Some(expected)
    {
        return (
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "unexpected content type\n",
        )
            .into_response();
    }
    let limits = &state.config.limits;
    let idle = limits.git_transport_idle_timeout.get();
    let max_bytes = limits.push_max_bytes.get();
    // The body also carries ref negotiation; leave headroom so that Git's own
    // receive.maxInputSize check, with its clear message, fires first.
    let body_limit = max_bytes.saturating_add(1024 * 1024);
    let git_dir = state.services.repo_path(access.repo.id);
    let shown = state
        .services
        .shown_mr_branches(&subject, access.repo.id, service.is_write())
        .await;
    let token = match service {
        Service::ReceivePack => {
            let grant = PushGrant {
                subject,
                repo_id: access.repo.id,
                transport: Transport::Http,
            };
            match state.hooks.mint(grant, crate::ipc::HOOK_TOKEN_TTL) {
                Ok(t) => Some(t),
                Err(e) => return internal(&e),
            }
        }
        Service::UploadPack => None,
    };
    let hooks = token.as_ref().map(|t| HookEnv {
        token: t.as_str(),
        ipc_socket: &state.ipc_socket,
        max_input_bytes: Some(max_bytes),
    });
    let mut child = match transport::spawn_http_rpc(
        &state.runner,
        &git_dir,
        service,
        git_protocol(headers),
        hooks.as_ref(),
        &shown,
    )
    .await
    {
        Ok(c) => c,
        Err(e) => return internal(&e),
    };
    let (mut stdin, stdout, stderr) = match child.take_stdio() {
        Ok(s) => s,
        Err(e) => return internal(&e),
    };

    let gzip = headers
        .get(header::CONTENT_ENCODING)
        .is_some_and(|v| v.as_bytes().eq_ignore_ascii_case(b"gzip"));
    let raw = StreamReader::new(body.into_data_stream().map_err(io::Error::other));
    let input: Box<dyn AsyncRead + Send + Unpin> = if gzip {
        Box::new(GzipDecoder::new(BufReader::new(raw)))
    } else {
        Box::new(raw)
    };
    tokio::spawn(async move {
        if let Err(e) = copy_bounded(input, &mut stdin, idle, body_limit).await {
            tracing::debug!(error = %e, "git transport: request body ended early");
        }
        let _ = stdin.shutdown().await;
    });
    tokio::spawn(async move {
        let mut stderr = stderr;
        let mut buf = Vec::new();
        let _ = tokio::io::AsyncReadExt::read_to_end(&mut stderr, &mut buf).await;
        if !buf.is_empty() {
            tracing::debug!(stderr = %String::from_utf8_lossy(&buf[..buf.len().min(4096)]), "git transport stderr");
        }
    });

    // The stream owns the child and the hook token: both live exactly as long
    // as the response, and the process group dies if the client disconnects
    // or Git stays silent for longer than the idle timeout.
    let out = ReaderStream::new(stdout);
    let stream = futures_util::stream::unfold(
        (out, Some((child, token))),
        move |(mut out, mut guard)| async move {
            let next = match tokio::time::timeout(idle, out.next()).await {
                Ok(n) => n,
                Err(_) => Some(Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "git transport idle timeout",
                ))),
            };
            match next {
                Some(Ok(chunk)) => Some((Ok(chunk), (out, guard))),
                Some(Err(e)) => {
                    // Dropping the guard kills the process group.
                    drop(guard.take());
                    Some((Err(e), (out, None)))
                }
                None => {
                    if let Some((mut child, _token)) = guard.take() {
                        match child.wait().await {
                            Ok(status) if !status.success() => {
                                tracing::debug!(%status, "git transport exited unsuccessfully")
                            }
                            Ok(_) => {}
                            Err(e) => tracing::warn!(error = %e, "git transport wait failed"),
                        }
                    }
                    None
                }
            }
        },
    )
    .map(|r: io::Result<Bytes>| r);
    let ct = match service {
        Service::UploadPack => "application/x-git-upload-pack-result",
        Service::ReceivePack => "application/x-git-receive-pack-result",
    };
    (no_cache(ct), Body::from_stream(stream)).into_response()
}
