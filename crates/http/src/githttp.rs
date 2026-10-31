// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Git smart HTTP without CGI.
//!
//! `info/refs` buffers the (capped) advertisement. The RPC endpoints stream:
//! the request body (gzip-decoded if needed) is piped into Git's stdin while
//! Git's stdout streams back as the response. If the client goes away, the
//! response stream is dropped, which kills the Git process group.

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
use slath_git::transport::{self, HookEnv, Service};
use subtle::ConstantTimeEq;
use tokio::io::{AsyncRead, AsyncWriteExt, BufReader};
use tokio_util::io::{ReaderStream, StreamReader};

use crate::{AppState, RepoEntry};

/// How long a hook token stays valid (`limits.git_transport_idle_timeout`).
const HOOK_TOKEN_TTL: Duration = Duration::from_secs(600);

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/{ns}/{repo}/info/refs", get(info_refs))
        .route("/{ns}/{repo}/git-upload-pack", post(upload_pack))
        .route("/{ns}/{repo}/git-receive-pack", post(receive_pack))
}

#[derive(Deserialize)]
struct InfoRefsQuery {
    service: Option<String>,
}

/// Why a request was refused. Missing or wrong credentials give the same 401
/// for existing and non-existent repositories.
#[derive(Debug, Clone, Copy)]
enum Denied {
    Unauthorized,
    NotFound,
}

impl IntoResponse for Denied {
    fn into_response(self) -> Response {
        match self {
            Self::Unauthorized => unauthorized(),
            Self::NotFound => not_found(),
        }
    }
}

fn authorize<'a>(
    state: &'a AppState,
    headers: &HeaderMap,
    ns: &str,
    repo: &str,
) -> Result<&'a RepoEntry, Denied> {
    if !valid_credentials(state, headers) {
        return Err(Denied::Unauthorized);
    }
    let name = repo.strip_suffix(".git").unwrap_or(repo);
    state.repo(ns, name).ok_or(Denied::NotFound)
}

fn valid_credentials(state: &AppState, headers: &HeaderMap) -> bool {
    let Some(value) = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
    else {
        return false;
    };
    let Some(encoded) = value.strip_prefix("Basic ") else {
        return false;
    };
    let Ok(decoded) = base64::engine::general_purpose::STANDARD.decode(encoded.trim()) else {
        return false;
    };
    let Some(colon) = decoded.iter().position(|&b| b == b':') else {
        return false;
    };
    let (user, password) = (&decoded[..colon], &decoded[colon + 1..]);
    !user.is_empty() && bool::from(password.ct_eq(state.token.expose().as_bytes()))
}

fn unauthorized() -> Response {
    (
        StatusCode::UNAUTHORIZED,
        [(header::WWW_AUTHENTICATE, "Basic realm=\"Slath\"")],
        "authentication required\n",
    )
        .into_response()
}

fn not_found() -> Response {
    (StatusCode::NOT_FOUND, "repository not found\n").into_response()
}

fn internal(e: &dyn std::fmt::Display) -> Response {
    tracing::error!(error = %e, "git transport failed");
    (StatusCode::INTERNAL_SERVER_ERROR, "internal error\n").into_response()
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
        return not_found();
    };
    let entry = match authorize(&state, &headers, &ns, &repo) {
        Ok(e) => e,
        Err(d) => return d.into_response(),
    };
    match transport::http_info_refs(
        &state.runner,
        &entry.git_dir,
        service,
        git_protocol(&headers),
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

async fn rpc(
    state: AppState,
    service: Service,
    ns: &str,
    repo: &str,
    headers: &HeaderMap,
    body: Body,
) -> Response {
    let entry = match authorize(&state, headers, ns, repo) {
        Ok(e) => e.clone(),
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

    let token = match service {
        Service::ReceivePack => match state.hooks.mint(entry.git_dir.clone(), HOOK_TOKEN_TTL) {
            Ok(t) => Some(t),
            Err(e) => return internal(&e),
        },
        Service::UploadPack => None,
    };
    let hooks = token.as_ref().map(|t| HookEnv {
        token: t.as_str(),
        ipc_socket: &state.ipc_socket,
    });
    let mut child = match transport::spawn_http_rpc(
        &state.runner,
        &entry.git_dir,
        service,
        git_protocol(headers),
        hooks.as_ref(),
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
    let mut input: Box<dyn AsyncRead + Send + Unpin> = if gzip {
        Box::new(GzipDecoder::new(BufReader::new(raw)))
    } else {
        Box::new(raw)
    };
    tokio::spawn(async move {
        if let Err(e) = tokio::io::copy(&mut input, &mut stdin).await {
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
    // as the response, and the process group dies if the client disconnects.
    let out = ReaderStream::new(stdout);
    let stream = futures_util::stream::unfold(
        (out, Some((child, token))),
        |(mut out, mut guard)| async move {
            match out.next().await {
                Some(chunk) => Some((chunk, (out, guard))),
                None => {
                    if let Some((mut child, _token)) = guard.take() {
                        match child.wait().await {
                            Ok(status) if !status.success() => {
                                tracing::debug!(%status, "git transport exited unsuccessfully");
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
