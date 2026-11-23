// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Agent context records: an agent's account of how it made a change,
//! uploaded through the API or MCP and shown next to the change to the
//! people who may see it.
//!
//! Storage: the metadata is a row; the payload is a blob encrypted with a
//! key of its own, and that key is sealed with the instance's master key
//! in the row, so rotating the master key re-encrypts only the key.
//! Nothing of a record is ever written to a repository's Git objects.
//!
//! Who sees a record: people with push permission on the repository, the
//! person the record names as having asked for the work, and the
//! uploader's owner (a bot's owner, or the uploader themselves), each only
//! while they can read the repository. Everyone else gets "not found".

use slath_core::authz::{Action, Subject, UserKind, UserState};
use slath_core::context_record::{Binding, Detail, Payload, Upload};
use slath_core::ids::{MergeRequestId, UserId};
use slath_core::secret_scan::Tier;
use slath_db::context_records::{Filter, NewRecord, Record};
use tokio::io::AsyncReadExt;

use crate::Services;
use crate::audit::Target;
use crate::blobs::BlobStore;
use crate::error::{FieldError, Result, ServiceError};
use crate::repos::RepoAccess;

const TABLE: &str = "context_records";
const KEY_COLUMN: &str = "payload_key_enc";

/// A record as its readers see it.
#[derive(Debug, Clone)]
pub struct ContextRecord {
    pub record: Record,
    pub uploader: Option<String>,
    pub uploader_is_bot: bool,
    pub initiated_by: Option<String>,
    /// The bound merge request's number and change display ID.
    pub merge_request: Option<i32>,
    pub change: Option<String>,
    /// Loaded only for a single record.
    pub payload: Option<Payload>,
}

fn aad(id: uuid::Uuid) -> Vec<u8> {
    format!("slath/context-payload/v1\0{id}").into_bytes()
}

fn user_id(subject: &Subject) -> Result<UserId> {
    match subject {
        Subject::User { id, .. } => Ok(*id),
        _ => Err(ServiceError::Unauthenticated),
    }
}

impl Services {
    /// A repository's context settings: (detail level, Entire import on).
    #[must_use]
    pub fn context_settings(a: &RepoAccess) -> (Detail, bool) {
        (
            Detail::parse(&a.repo.context_detail).unwrap_or(Detail::Summary),
            a.repo.context_entire_import,
        )
    }

    /// Whether `subject` may see records of this repository at all (people
    /// with push permission); others may still see records naming them.
    fn context_writer(subject: &Subject, a: &RepoAccess) -> bool {
        a.can(subject, Action::RepoPush)
    }

    async fn may_see(&self, subject: &Subject, a: &RepoAccess, r: &Record) -> Result<bool> {
        if Self::context_writer(subject, a) {
            return Ok(true);
        }
        let Ok(viewer) = user_id(subject) else {
            return Ok(false);
        };
        if r.initiated_by == Some(viewer) {
            return Ok(true);
        }
        match r.uploader_id {
            Some(u) => Ok(slath_db::context_records::owns_uploader(&self.pool, u, viewer).await?),
            None => Ok(false),
        }
    }

    /// Uploads a record (`body` is its JSON). Needs push permission.
    pub async fn upload_context_record(
        &self,
        subject: &Subject,
        ns: &str,
        repo: &str,
        body: &[u8],
    ) -> Result<ContextRecord> {
        let a = self
            .repo_access(subject, ns, repo, Action::RepoPush)
            .await?;
        self.ingest_context_record(subject, &a, body).await
    }

    /// Imports one session of an Entire CLI checkpoint (experimental):
    /// converts it into a record and ingests that like an upload. "Not
    /// found" while the repository's Entire import is off.
    pub async fn import_entire_checkpoint(
        &self,
        subject: &Subject,
        ns: &str,
        repo: &str,
        body: &[u8],
    ) -> Result<ContextRecord> {
        let a = self
            .repo_access(subject, ns, repo, Action::RepoPush)
            .await?;
        let (detail, entire) = Self::context_settings(&a);
        if !entire {
            return Err(ServiceError::NotFound);
        }
        let limit = self.config.limits.context_record_max_bytes.0;
        if body.len() as u64 > limit.saturating_mul(4) {
            return Err(ServiceError::invalid(
                "body",
                "too_large",
                format!("Send a checkpoint of at most {} KiB.", limit * 4 / 1024),
            ));
        }
        let import: slath_core::entire_import::Import =
            serde_json::from_slice(body).map_err(|e| {
                ServiceError::invalid(
                    "body",
                    "invalid",
                    format!("This isn't a checkpoint session: {e}."),
                )
            })?;
        let record = serde_json::to_vec(&import.to_upload(detail == Detail::Full))
            .map_err(ServiceError::internal)?;
        self.ingest_context_record(subject, &a, &record).await
    }

    pub(crate) async fn ingest_context_record(
        &self,
        subject: &Subject,
        a: &RepoAccess,
        body: &[u8],
    ) -> Result<ContextRecord> {
        let uploader = user_id(subject)?;
        let limits = &self.config.limits;
        if body.len() as u64 > limits.context_record_max_bytes.0 {
            return Err(ServiceError::invalid(
                "body",
                "too_large",
                format!(
                    "Send a record of at most {} KiB.",
                    limits.context_record_max_bytes.0 / 1024
                ),
            ));
        }
        let upload: Upload = serde_json::from_slice(body).map_err(|e| {
            ServiceError::invalid(
                "body",
                "invalid",
                format!("This isn't a valid record: {e}."),
            )
        })?;
        let (detail, _) = Self::context_settings(a);
        upload
            .check(detail == Detail::Full, limits.context_transcript_entries)
            .map_err(|errs| {
                ServiceError::Invalid(
                    errs.into_iter()
                        .map(|e| FieldError::new(&e.field, "invalid", e.message))
                        .collect(),
                )
            })?;
        // Secrets are refused whatever the repository's push protection
        // mode: nothing of the request is stored.
        let mut secrets = Vec::new();
        for (field, text) in upload.texts() {
            for f in self.scan_rules.scan(text.as_bytes()) {
                if f.tier == Tier::Block {
                    secrets.push(FieldError::new(
                        &field,
                        "secret",
                        format!(
                            "Line {}, column {}: {} ({}). Remove it and upload again.",
                            f.line, f.column, f.description, f.excerpt
                        ),
                    ));
                }
            }
        }
        if !secrets.is_empty() {
            return Err(ServiceError::Invalid(secrets));
        }
        let initiated_by = match &upload.initiated_by {
            None => None,
            Some(name) => Some(self.initiating_user(a, name).await?),
        };
        let (mr_id, change_id, state) = match &upload.binding {
            None => (None, None, "none"),
            Some(b) => match self.context_binding(subject, a, uploader, b).await? {
                Some((m, c)) => (Some(m), c, "bound"),
                None => (None, None, "unlinked"),
            },
        };

        let id = uuid::Uuid::now_v7();
        let payload = upload.payload();
        let plain = serde_json::to_vec(&payload).map_err(ServiceError::internal)?;
        let (key, ciphertext) =
            slath_auth::crypto::data_key::seal(&plain, &aad(id)).map_err(ServiceError::internal)?;
        let id_text = id.to_string();
        let sealed_key = self
            .envelope
            .seal(
                key.expose(),
                slath_auth::crypto::Location {
                    table: TABLE,
                    column: KEY_COLUMN,
                    row: &id_text,
                },
            )
            .map_err(ServiceError::internal)?;
        slath_db::secrets::register_key(&self.pool, &sealed_key.key_id).await?;
        let mut tx = self.pool.begin().await.map_err(ServiceError::internal)?;
        let mut w = self
            .blobs
            .writer(ciphertext.len() as u64)
            .await
            .map_err(ServiceError::internal)?;
        w.write(&ciphertext).await.map_err(ServiceError::internal)?;
        let staged = w.finish().await.map_err(ServiceError::internal)?;
        let sha = self.store_blob(&mut tx, staged).await?;
        let b = upload.binding.as_ref();
        slath_db::context_records::insert(
            &mut tx,
            &NewRecord {
                id,
                repo_id: a.repo.id,
                uploader_id: uploader,
                initiated_by,
                agent_name: &upload.agent.name,
                agent_model: upload.agent.model.as_deref(),
                agent_tool: upload.agent.tool.as_deref(),
                detail: detail.as_str(),
                requested_mr: b.and_then(|b| b.merge_request),
                requested_change: b.and_then(|b| b.change.as_deref()),
                requested_commit: b.and_then(|b| b.commit.as_deref()),
                mr_id,
                change_id,
                binding_state: state,
                payload_sha256: &sha,
                payload_bytes: i32::try_from(ciphertext.len()).unwrap_or(i32::MAX),
                transcript_entries: i32::try_from(payload.transcript.len()).unwrap_or(i32::MAX),
                payload_key_enc: &sealed_key.ciphertext,
                key_id: &sealed_key.key_id,
            },
        )
        .await?;
        tx.commit().await.map_err(ServiceError::internal)?;
        self.audit(
            subject,
            "context.create",
            Target::new("repository", Some(*a.repo.id.as_uuid())),
            serde_json::json!({"record": id, "binding": state}),
        )
        .await;
        let record = slath_db::context_records::get(&self.pool, a.repo.id, id)
            .await?
            .ok_or(ServiceError::NotFound)?;
        let mut view = self.context_view(record).await?;
        view.payload = Some(payload);
        Ok(view)
    }

    /// The person a record names as having asked for the work: an active
    /// person who can read the repository.
    async fn initiating_user(&self, a: &RepoAccess, name: &str) -> Result<UserId> {
        let refuse = || {
            ServiceError::invalid(
                "/initiated_by",
                "invalid",
                "Name a person who can read this repository.",
            )
        };
        let u = slath_db::users::get_by_name(&self.pool, crate::one_name(name))
            .await?
            .ok_or_else(refuse)?;
        if u.kind != UserKind::Human || u.state != UserState::Active {
            return Err(refuse());
        }
        let s = crate::notifications::user_subject(u.id, &u);
        match self
            .repo_access_by_id(&s, a.repo.id, Action::RepoRead)
            .await
        {
            Ok(_) => Ok(u.id),
            Err(ServiceError::NotFound | ServiceError::Forbidden) => Err(refuse()),
            Err(e) => Err(e),
        }
    }

    /// Whether `uploader` may update merge request `m`: its author, or
    /// someone who may update its source branch.
    async fn may_update_mr(
        &self,
        subject: &Subject,
        a: &RepoAccess,
        uploader: UserId,
        m: &slath_db::merge_requests::MergeRequest,
    ) -> Result<bool> {
        Ok(m.author_id == Some(uploader) || self.may_write_source(subject, a, m).await?)
    }

    /// The merge request and change a binding names, if it exists and the
    /// uploader may update that merge request.
    async fn context_binding(
        &self,
        subject: &Subject,
        a: &RepoAccess,
        uploader: UserId,
        b: &Binding,
    ) -> Result<Option<(MergeRequestId, Option<uuid::Uuid>)>> {
        let (m, change) = if let Some(n) = b.merge_request {
            let Some(m) = slath_db::merge_requests::get(&self.pool, a.repo.id, n).await? else {
                return Ok(None);
            };
            let change = match &b.change {
                None => None,
                Some(d) => {
                    let Some(c) = slath_db::mr_changes::changes(&self.pool, m.id)
                        .await?
                        .into_iter()
                        .find(|c| &c.display_id == d)
                    else {
                        return Ok(None);
                    };
                    Some(c.id)
                }
            };
            (m, change)
        } else if let Some(oid) = &b.commit {
            let found =
                slath_db::context_records::open_changes_with_commit(&self.pool, a.repo.id, oid)
                    .await?;
            let [(mr, change)] = found.as_slice() else {
                return Ok(None);
            };
            let Some(m) = slath_db::merge_requests::get_by_id(&self.pool, *mr).await? else {
                return Ok(None);
            };
            (m, Some(*change))
        } else {
            return Ok(None);
        };
        if !self.may_update_mr(subject, a, uploader, &m).await? {
            return Ok(None);
        }
        Ok(Some((m.id, change)))
    }

    async fn context_view(&self, record: Record) -> Result<ContextRecord> {
        let user = |id: Option<UserId>| async move {
            match id {
                Some(id) => slath_db::users::get(&self.pool, id).await,
                None => Ok(None),
            }
        };
        let uploader = user(record.uploader_id).await?;
        let initiated_by = user(record.initiated_by).await?.map(|u| u.name);
        let (merge_request, change) = match record.mr_id {
            Some(m) => {
                let number = slath_db::merge_requests::get_by_id(&self.pool, m)
                    .await?
                    .map(|m| m.number);
                let change = match record.change_id {
                    Some(c) => slath_db::mr_changes::changes(&self.pool, m)
                        .await?
                        .into_iter()
                        .find(|x| x.id == c)
                        .map(|x| x.display_id),
                    None => None,
                };
                (number, change)
            }
            None => (None, None),
        };
        Ok(ContextRecord {
            uploader_is_bot: uploader.as_ref().is_some_and(|u| u.kind == UserKind::Bot),
            uploader: uploader.map(|u| u.name),
            initiated_by,
            merge_request,
            change,
            payload: None,
            record,
        })
    }

    async fn context_payload(&self, r: &Record) -> Result<Payload> {
        let id_text = r.id.to_string();
        let key = self
            .envelope
            .open(
                &r.payload_key_enc,
                &r.key_id,
                slath_auth::crypto::Location {
                    table: TABLE,
                    column: KEY_COLUMN,
                    row: &id_text,
                },
            )
            .map_err(ServiceError::internal)?;
        let mut file = self
            .blobs
            .open(&r.payload_sha256)
            .await
            .map_err(ServiceError::internal)?
            .ok_or_else(|| {
                ServiceError::Internal("a context record's payload is missing".into())
            })?;
        let mut ciphertext = Vec::new();
        file.read_to_end(&mut ciphertext)
            .await
            .map_err(ServiceError::internal)?;
        let plain = slath_auth::crypto::data_key::open(key.expose(), &ciphertext, &aad(r.id))
            .map_err(ServiceError::internal)?;
        serde_json::from_slice(plain.expose()).map_err(ServiceError::internal)
    }

    /// One record with its payload; "not found" for anyone who may not see it.
    pub async fn context_record(
        &self,
        subject: &Subject,
        ns: &str,
        repo: &str,
        id: uuid::Uuid,
    ) -> Result<ContextRecord> {
        let a = self
            .repo_access(subject, ns, repo, Action::RepoRead)
            .await?;
        let r = slath_db::context_records::get(&self.pool, a.repo.id, id)
            .await?
            .ok_or(ServiceError::NotFound)?;
        if !self.may_see(subject, &a, &r).await? {
            return Err(ServiceError::NotFound);
        }
        let payload = self.context_payload(&r).await?;
        let mut view = self.context_view(r).await?;
        view.payload = Some(payload);
        Ok(view)
    }

    /// A repository's records the subject may see, newest first, without
    /// payloads. `limit` + 1 are read to tell whether more follow.
    pub async fn context_records(
        &self,
        subject: &Subject,
        a: &RepoAccess,
        filter: Filter,
        limit: usize,
    ) -> Result<Vec<ContextRecord>> {
        let mut out = Vec::new();
        let mut f = filter;
        // Others than writers see only some records; read in pages until
        // the page is full or nothing is left.
        loop {
            let page = slath_db::context_records::list(
                &self.pool,
                a.repo.id,
                f,
                i64::try_from(limit + 1).unwrap_or(i64::MAX),
            )
            .await?;
            let last = page.last().map(|r| (r.created_at, r.id));
            let done = page.len() <= limit;
            for r in page {
                if out.len() > limit {
                    break;
                }
                if self.may_see(subject, a, &r).await? {
                    out.push(self.context_view(r).await?);
                }
            }
            if done || out.len() > limit {
                break;
            }
            f.before = last;
        }
        Ok(out)
    }

    /// The API's list: filters by merge request number and change display
    /// ID; at most `want` records.
    pub async fn list_context_records(
        &self,
        subject: &Subject,
        (ns, repo): (&str, &str),
        mr: Option<i32>,
        change: Option<&str>,
        unlinked: bool,
        want: usize,
    ) -> Result<Vec<ContextRecord>> {
        let a = self
            .repo_access(subject, ns, repo, Action::RepoRead)
            .await?;
        let mut f = Filter {
            unlinked,
            ..Filter::default()
        };
        if let Some(n) = mr {
            let Some(m) = slath_db::merge_requests::get(&self.pool, a.repo.id, n).await? else {
                return Ok(Vec::new());
            };
            f.mr = Some(m.id);
            if let Some(d) = change {
                let Some(c) = slath_db::mr_changes::changes(&self.pool, m.id)
                    .await?
                    .into_iter()
                    .find(|c| c.display_id == d)
                else {
                    return Ok(Vec::new());
                };
                f.change = Some(c.id);
            }
        } else if change.is_some() {
            return Err(ServiceError::invalid(
                "change",
                "invalid",
                "Name the merge request too.",
            ));
        }
        self.context_records(subject, &a, f, want).await
    }

    /// The records bound to merge request `m` that the subject may see,
    /// with payloads (oldest first), for the merge request's pages.
    pub async fn mr_context_records(
        &self,
        subject: &Subject,
        a: &RepoAccess,
        m: MergeRequestId,
    ) -> Result<Vec<ContextRecord>> {
        let rows = slath_db::context_records::list(
            &self.pool,
            a.repo.id,
            Filter {
                mr: Some(m),
                ..Filter::default()
            },
            200,
        )
        .await?;
        let mut out = Vec::new();
        for r in rows.into_iter().rev() {
            if self.may_see(subject, a, &r).await? {
                let payload = self.context_payload(&r).await?;
                let mut v = self.context_view(r).await?;
                v.payload = Some(payload);
                out.push(v);
            }
        }
        Ok(out)
    }

    /// Binds the records that commits of a new revision name in an
    /// `Agent-Session` trailer to the commits' changes (or to the merge
    /// request, for a commit without a resolved change): only records of
    /// the same repository that aren't bound yet, and only if their
    /// uploader may update the merge request now. Failures are logged; a
    /// revision is never refused over a trailer.
    pub(crate) async fn bind_agent_sessions(
        &self,
        m: &slath_db::merge_requests::MergeRequest,
        sessions: Vec<(String, Option<uuid::Uuid>)>,
    ) {
        for (value, change) in sessions {
            let Ok(id) = value.trim().parse::<uuid::Uuid>() else {
                continue;
            };
            if let Err(e) = self.bind_agent_session(m, id, change).await {
                tracing::warn!(error = %e, mr = %m.id, "binding an agent session trailer failed");
            }
        }
    }

    async fn bind_agent_session(
        &self,
        m: &slath_db::merge_requests::MergeRequest,
        id: uuid::Uuid,
        change: Option<uuid::Uuid>,
    ) -> Result<()> {
        let Some(r) = slath_db::context_records::get(&self.pool, m.repo_id, id).await? else {
            return Ok(());
        };
        let Some(uploader) = r.uploader_id else {
            return Ok(());
        };
        if r.binding_state == "bound" {
            return Ok(());
        }
        let Some(u) = slath_db::users::get(&self.pool, uploader).await? else {
            return Ok(());
        };
        if u.state != UserState::Active {
            return Ok(());
        }
        let s = crate::notifications::user_subject(uploader, &u);
        let a = match self
            .repo_access_by_id(&s, m.repo_id, Action::RepoRead)
            .await
        {
            Ok(a) => a,
            Err(ServiceError::NotFound | ServiceError::Forbidden) => return Ok(()),
            Err(e) => return Err(e),
        };
        if !self.may_update_mr(&s, &a, uploader, m).await? {
            return Ok(());
        }
        slath_db::context_records::bind(&self.pool, m.repo_id, id, m.id, change).await?;
        Ok(())
    }

    /// Changes a repository's context settings (repository admins).
    pub async fn set_context_settings(
        &self,
        subject: &Subject,
        a: &RepoAccess,
        detail: Option<&str>,
        entire_import: Option<bool>,
    ) -> Result<()> {
        if !a.can(subject, Action::RepoSettings) {
            return Err(ServiceError::Forbidden);
        }
        if let Some(d) = detail
            && Detail::parse(d).is_none()
        {
            return Err(ServiceError::invalid(
                "context_detail",
                "invalid",
                "Choose summary or full.",
            ));
        }
        slath_db::context_records::set_settings(&self.pool, a.repo.id, detail, entire_import)
            .await?;
        self.audit(
            subject,
            "repo.context_settings",
            Target::new("repository", Some(*a.repo.id.as_uuid())),
            serde_json::json!({"context_detail": detail, "context_entire_import": entire_import}),
        )
        .await;
        Ok(())
    }
}
