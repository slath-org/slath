// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Attachments: upload (API with a token, pages with the session and the
//! CSRF header) and serving with the raw-content headers, plus uploaded
//! avatars.

use axum::body::Body;
use axum::extract::{DefaultBodyLimit, FromRequest, Multipart, Path, Request, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::{Json, Router};
use serde::Serialize;
use slath_core::authz::Subject;
use slath_db::blobs::Attachment;
use slath_service::attachments::attachment_path;
use slath_service::blobs::WriteError;
use utoipa::ToSchema;

use crate::AppState;
use crate::api::ApiUser;
use crate::auth::MaybeSession;
use crate::error::AppError;
use crate::routes::{Access, RouteExt};

/// An uploaded attachment.
#[derive(Serialize, ToSchema)]
pub struct AttachmentJson {
    pub id: String,
    pub filename: String,
    /// The detected type: an image type, or `application/octet-stream`.
    pub content_type: String,
    pub size: i64,
    /// Path of the file on this instance.
    pub url: String,
    /// Markdown that embeds (images) or links (other files) the attachment.
    pub markdown: String,
}

fn json(ns: &str, repo: &str, a: &Attachment) -> AttachmentJson {
    let url = attachment_path(ns, repo, a);
    let label: String = a
        .filename
        .chars()
        .map(|c| if "[]\\".contains(c) { '_' } else { c })
        .collect();
    let markdown = if a.content_type.starts_with("image/") {
        format!("![{label}]({url})")
    } else {
        format!("[{label}]({url})")
    };
    AttachmentJson {
        id: a.id.to_string(),
        filename: a.filename.clone(),
        content_type: a.content_type.clone(),
        size: a.size_bytes,
        url,
        markdown,
    }
}

/// Parts of an upload form looked at before giving up on finding `file`.
const MAX_PARTS: usize = 8;

fn too_large(state: &AppState) -> AppError {
    AppError::with_code(StatusCode::PAYLOAD_TOO_LARGE, "too_large", "File too large").detail(
        format!(
            "Attachments may be at most {} MiB.",
            state.config.limits.attachment_max_bytes.0 / (1024 * 1024)
        ),
    )
}

/// Reads the `file` part of a multipart upload into the blob store, after
/// the subject's access was checked.
async fn upload(
    state: &AppState,
    subject: &Subject,
    ns: &str,
    repo: &str,
    req: Request,
) -> Result<(StatusCode, Json<AttachmentJson>), AppError> {
    let a = state
        .services
        .attachment_upload_access(subject, ns, repo)
        .await?;
    // The body is looked at only after the subject was authorized.
    let mut form = Multipart::from_request(req, state)
        .await
        .map_err(|_| AppError::bad_request("Send the file as multipart/form-data."))?;
    let missing = || {
        AppError::from(slath_service::ServiceError::invalid(
            "file",
            "required",
            "Choose a file.",
        ))
    };
    // Other parts are skipped, but only a few of them.
    for _ in 0..MAX_PARTS {
        let Some(mut field) = form
            .next_field()
            .await
            .map_err(|_| AppError::bad_request("The upload is not a valid multipart form."))?
        else {
            return Err(missing());
        };
        if field.name() != Some("file") {
            continue;
        }
        let filename = field.file_name().unwrap_or("file").to_owned();
        let mut w = state.services.attachment_writer().await?;
        loop {
            match field.chunk().await {
                Ok(Some(chunk)) => w.write(&chunk).await.map_err(|e| match e {
                    WriteError::TooLarge => too_large(state),
                    WriteError::Io(e) => AppError::internal(&e),
                })?,
                Ok(None) => break,
                Err(e) if e.status() == StatusCode::PAYLOAD_TOO_LARGE => {
                    return Err(too_large(state));
                }
                Err(_) => return Err(AppError::bad_request("The upload was interrupted.")),
            }
        }
        let staged = w.finish().await.map_err(|e| AppError::internal(&e))?;
        let att = state
            .services
            .create_attachment(subject, &a, &filename, staged)
            .await?;
        return Ok((
            StatusCode::CREATED,
            Json(json(&a.repo.namespace, &a.repo.name, &att)),
        ));
    }
    Err(missing())
}

/// Uploads a file (multipart field `file`) for use in issue and comment text.
#[utoipa::path(post, path = "/api/v1/repos/{ns}/{repo}/attachments", tag = "issues", security(("token" = ["issues:write"])), params(("ns" = String, Path), ("repo" = String, Path)),
    request_body(content_type = "multipart/form-data", description = "A `file` part."),
    responses((status = 201, body = AttachmentJson), (status = 404, body = crate::api::Problem), (status = 413, body = crate::api::Problem), (status = 422, body = crate::api::Problem)))]
pub async fn api_upload(
    State(state): State<AppState>,
    ApiUser(s): ApiUser,
    Path((ns, repo)): Path<(String, String)>,
    req: Request,
) -> Result<(StatusCode, Json<AttachmentJson>), AppError> {
    upload(&state, &s, &ns, &repo, req).await
}

/// `POST /{ns}/{repo}/-/attachments`: the same for the editor (session plus
/// the `X-CSRF-Token` header).
async fn ui_upload(
    State(state): State<AppState>,
    session: MaybeSession,
    Path((ns, repo)): Path<(String, String)>,
    req: Request,
) -> Result<Response, AppError> {
    let s = session.subject();
    if matches!(s, Subject::Anonymous) {
        return Err(AppError::unauthenticated());
    }
    Ok(upload(&state, &s, &ns, &repo, req).await?.into_response())
}

/// A file name for `Content-Disposition`: ASCII fallback plus the UTF-8 name.
fn disposition(kind: &str, name: &str) -> Option<HeaderValue> {
    let ascii: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || "._- ".contains(c) {
                c
            } else {
                '_'
            }
        })
        .collect();
    let mut encoded = String::new();
    for b in name.bytes() {
        if b.is_ascii_alphanumeric() || b"._-".contains(&b) {
            encoded.push(char::from(b));
        } else {
            encoded.push_str(&format!("%{b:02X}"));
        }
    }
    HeaderValue::from_str(&format!(
        "{kind}; filename=\"{ascii}\"; filename*=UTF-8''{encoded}"
    ))
    .ok()
}

/// `GET /{ns}/{repo}/attachments/{id}/{filename}`.
async fn serve(
    State(state): State<AppState>,
    session: MaybeSession,
    Path((ns, repo, id, _name)): Path<(String, String, String, String)>,
) -> Result<Response, AppError> {
    let subject = session.subject();
    let id: uuid::Uuid = id.parse().map_err(|_| AppError::not_found())?;
    let (a, att, file) = state.services.attachment(&subject, &ns, &repo, id).await?;
    let inline = att.content_type.starts_with("image/");
    let ct = if inline {
        att.content_type.as_str()
    } else {
        "application/octet-stream"
    };
    let mut res = Body::from_stream(tokio_util::io::ReaderStream::new(file)).into_response();
    let h = res.headers_mut();
    if let Ok(v) = HeaderValue::from_str(ct) {
        h.insert(header::CONTENT_TYPE, v);
    }
    if let Ok(v) = HeaderValue::from_str(&att.size_bytes.to_string()) {
        h.insert(header::CONTENT_LENGTH, v);
    }
    if let Some(v) = disposition(if inline { "inline" } else { "attachment" }, &att.filename) {
        h.insert(header::CONTENT_DISPOSITION, v);
    }
    h.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static("sandbox; default-src 'none'; style-src 'unsafe-inline'"),
    );
    h.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    let cache = if a.repo.visibility != slath_core::authz::Visibility::Public {
        "private, no-store"
    } else {
        "private, max-age=3600"
    };
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static(cache));
    Ok(res)
}

/// `GET /-/avatars/{sha256}.png`: an uploaded avatar (content-addressed, so
/// it never changes).
pub async fn avatar_png(state: &AppState, sha: &str) -> Result<Response, AppError> {
    let file = state.services.avatar_file(sha).await?;
    let mut res = Body::from_stream(tokio_util::io::ReaderStream::new(file)).into_response();
    let h = res.headers_mut();
    h.insert(header::CONTENT_TYPE, HeaderValue::from_static("image/png"));
    h.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("public, max-age=31536000, immutable"),
    );
    h.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static("sandbox; default-src 'none'"),
    );
    h.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    Ok(res)
}

/// The fields of an avatar form.
pub struct AvatarForm {
    /// The uploaded image, if a file was chosen.
    pub image: Option<Vec<u8>>,
    /// The "remove" button was used.
    pub remove: bool,
}

/// Reads an avatar form (`avatar` file part, optional `remove` field). The
/// body was already bounded by the CSRF check.
pub async fn avatar_form(mut form: Multipart) -> Result<AvatarForm, AppError> {
    let mut out = AvatarForm {
        image: None,
        remove: false,
    };
    let bad = || AppError::bad_request("The upload is not a valid multipart form.");
    while let Some(field) = form.next_field().await.map_err(|_| bad())? {
        match field.name() {
            Some("avatar") => {
                let data = field.bytes().await.map_err(|_| bad())?;
                if !data.is_empty() {
                    out.image = Some(data.to_vec());
                }
            }
            Some("remove") => out.remove = true,
            _ => {}
        }
    }
    Ok(out)
}

/// Attachment routes. The general body limit doesn't apply to uploads; the
/// blob writer enforces `attachment_max_bytes` while streaming.
pub fn ui_router() -> Router<AppState> {
    use axum::routing::{get, post};
    Router::new()
        .route_as(
            "/{ns}/{repo}/-/attachments",
            Access::Resource,
            post(ui_upload).layer(DefaultBodyLimit::disable()),
        )
        .route_as(
            "/{ns}/{repo}/attachments/{id}/{name}",
            Access::Resource,
            get(serve),
        )
}

pub fn api_router() -> Router<AppState> {
    use axum::routing::post;
    Router::new().route_as(
        "/api/v1/repos/{ns}/{repo}/attachments",
        Access::Resource,
        post(api_upload).layer(DefaultBodyLimit::disable()),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disposition_is_safe() {
        let v = disposition("attachment", "a\"b;\r\nc ü.svg").unwrap();
        let s = v.to_str().unwrap();
        assert!(s.starts_with("attachment; filename=\"a_b___c _.svg\"; filename*=UTF-8''"));
        assert!(!s.contains('\n'));
    }
}
