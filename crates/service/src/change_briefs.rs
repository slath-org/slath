// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Change briefs: what the author of a change version says about it (what
//! it's for, how it does it, how it was checked, and what's missing),
//! shown next to the code as its author's claim.
//!
//! A brief belongs to a change and one of its versions' commits, so a
//! revision that keeps a change's commit keeps its brief. Writing again
//! adds an edit; a version's brief is its newest edit. Everyone who can
//! read the merge request reads its briefs; its author and those who may
//! push its source branch write them, while it's open. Every field is
//! scanned for secrets before anything is stored.

use serde::Deserialize;
use slath_core::authz::{Subject, UserKind};
use slath_db::change_briefs::{Brief, NewBrief};
use slath_db::merge_requests::MergeRequest;

use crate::Services;
use crate::error::{Result, ServiceError};
use crate::mr_changes::ChangeInfo;
use crate::repos::RepoAccess;

/// The one schema this version accepts.
pub const SCHEMA: &str = "slath-brief/1";
/// The longest field, in bytes.
pub const FIELD_MAX_BYTES: usize = 16 * 1024;

/// A brief as its readers see it.
#[derive(Debug, Clone)]
pub struct ChangeBrief {
    pub brief: Brief,
    pub author: Option<String>,
    pub author_is_bot: bool,
    /// The number of the change's first version with this commit.
    pub version: Option<usize>,
}

/// A brief as written (`slath-brief/1`).
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Upload {
    pub schema: String,
    pub intent: String,
    #[serde(default)]
    pub approach: Option<String>,
    #[serde(default)]
    pub verification: Option<String>,
    #[serde(default)]
    pub gaps: Option<String>,
    /// A version's commit; the change's latest version without it.
    #[serde(default)]
    pub commit: Option<String>,
}

/// An optional field, with empty text counting as absent.
fn present(s: Option<&String>) -> Option<&str> {
    s.map(|s| s.as_str()).filter(|s| !s.trim().is_empty())
}

impl Upload {
    /// Its fields by JSON pointer, absent ones left out.
    fn fields(&self) -> Vec<(String, &str)> {
        [
            ("/intent", Some(self.intent.as_str())),
            ("/approach", present(self.approach.as_ref())),
            ("/verification", present(self.verification.as_ref())),
            ("/gaps", present(self.gaps.as_ref())),
        ]
        .into_iter()
        .filter_map(|(k, v)| v.map(|v| (k.to_owned(), v)))
        .collect()
    }

    fn check(&self) -> Result<()> {
        if self.schema != SCHEMA {
            return Err(ServiceError::invalid(
                "/schema",
                "schema",
                format!("Send \"schema\": \"{SCHEMA}\"."),
            ));
        }
        if self.intent.trim().is_empty() {
            return Err(ServiceError::invalid(
                "/intent",
                "invalid",
                "Say what the change is for.",
            ));
        }
        let long: Vec<_> = self
            .fields()
            .into_iter()
            .filter(|(_, v)| v.len() > FIELD_MAX_BYTES)
            .map(|(k, _)| {
                crate::error::FieldError::new(
                    &k,
                    "too_long",
                    format!("Keep it to {} KiB.", FIELD_MAX_BYTES / 1024),
                )
            })
            .collect();
        if long.is_empty() {
            Ok(())
        } else {
            Err(ServiceError::Invalid(long))
        }
    }
}

impl Services {
    /// The change `id` names (display ID, a prefix of at least 8
    /// characters, or a version's commit) with its row ID.
    async fn brief_change(&self, m: &MergeRequest, id: &str) -> Result<(uuid::Uuid, ChangeInfo)> {
        let info = self.mr_change(m, id).await?;
        let row = slath_db::mr_changes::changes(&self.pool, m.id)
            .await?
            .into_iter()
            .find(|c| c.display_id == info.display_id)
            .ok_or(ServiceError::NotFound)?;
        Ok((row.id, info))
    }

    /// Whether `subject` may write briefs of `m` now: it's open, and they
    /// are its author or may push to the repository (the rule for
    /// rebinding its commits, so token scopes and archiving apply).
    #[must_use]
    pub fn may_write_brief(subject: &Subject, a: &RepoAccess, m: &MergeRequest) -> bool {
        m.state == "open" && Self::can_rebind(subject, a, m)
    }

    /// Writes a brief (`body` is its JSON) for a version of a change.
    pub async fn write_change_brief(
        &self,
        subject: &Subject,
        (ns, repo, number): (&str, &str, i32),
        change: &str,
        body: &[u8],
    ) -> Result<ChangeBrief> {
        let (a, m) = self.mr(subject, ns, repo, number).await?;
        let (change_id, info) = self.brief_change(&m, change).await?;
        let Subject::User { id: author, .. } = subject else {
            return Err(ServiceError::Unauthenticated);
        };
        if !Self::can_rebind(subject, &a, &m) {
            return Err(ServiceError::Forbidden);
        }
        if m.state != "open" {
            return Err(ServiceError::Conflict(
                "This merge request isn't open, so its briefs can't change.".into(),
            ));
        }
        let limit = self.config.limits.brief_max_bytes.0;
        if body.len() as u64 > limit {
            return Err(ServiceError::invalid(
                "body",
                "too_large",
                format!("Send a brief of at most {} KiB.", limit / 1024),
            ));
        }
        let upload: Upload = serde_json::from_slice(body).map_err(|e| {
            ServiceError::invalid("body", "invalid", format!("This isn't a valid brief: {e}."))
        })?;
        upload.check()?;
        let commit = match &upload.commit {
            None => info
                .versions
                .last()
                .map(|v| v.commit_oid.clone())
                .ok_or(ServiceError::NotFound)?,
            Some(c) => info
                .versions
                .iter()
                .find(|v| &v.commit_oid == c)
                .map(|v| v.commit_oid.clone())
                .ok_or_else(|| {
                    ServiceError::invalid(
                        "/commit",
                        "commit",
                        "Name the commit of one of this change's versions.",
                    )
                })?,
        };
        self.refuse_secrets(upload.fields())?;
        let origin = self.origin_of(subject, &a.repo).await?;
        let brief = slath_db::change_briefs::insert(
            &self.pool,
            &NewBrief {
                mr_id: m.id,
                change_id,
                commit_oid: &commit,
                author_id: *author,
                origin: origin.as_str(),
                intent: upload.intent.trim_end(),
                approach: present(upload.approach.as_ref()).map(str::trim_end),
                verification: present(upload.verification.as_ref()).map(str::trim_end),
                gaps: present(upload.gaps.as_ref()).map(str::trim_end),
            },
        )
        .await?;
        self.brief_view(&info, brief).await
    }

    async fn brief_view(&self, info: &ChangeInfo, brief: Brief) -> Result<ChangeBrief> {
        let author = match brief.author_id {
            Some(id) => slath_db::users::get(&self.pool, id).await?,
            None => None,
        };
        Ok(ChangeBrief {
            version: info
                .versions
                .iter()
                .find(|v| v.commit_oid == brief.commit_oid)
                .map(|v| v.number),
            author_is_bot: author.as_ref().is_some_and(|u| u.kind == UserKind::Bot),
            author: author.map(|u| u.name),
            brief,
        })
    }

    /// A change's briefs, newest first, for everyone who can read the
    /// merge request; with `commit`, only that version's.
    pub async fn change_briefs(
        &self,
        subject: &Subject,
        (ns, repo, number): (&str, &str, i32),
        change: &str,
        commit: Option<&str>,
        want: usize,
    ) -> Result<Vec<ChangeBrief>> {
        let (_, m) = self.mr(subject, ns, repo, number).await?;
        self.mr_change_briefs(&m, change, commit, want).await
    }

    /// The same for a merge request the caller may read.
    pub async fn mr_change_briefs(
        &self,
        m: &MergeRequest,
        change: &str,
        commit: Option<&str>,
        want: usize,
    ) -> Result<Vec<ChangeBrief>> {
        let (change_id, info) = self.brief_change(m, change).await?;
        let rows = slath_db::change_briefs::list(
            &self.pool,
            change_id,
            commit,
            i64::try_from(want).unwrap_or(i64::MAX),
        )
        .await?;
        let mut out = Vec::with_capacity(rows.len());
        for b in rows {
            out.push(self.brief_view(&info, b).await?);
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn upload(json: &str) -> std::result::Result<Upload, serde_json::Error> {
        serde_json::from_str(json)
    }

    #[test]
    fn briefs_need_the_schema_and_an_intent() {
        assert!(upload(r#"{"schema":"slath-brief/1","intent":"x","extra":1}"#).is_err());
        let wrong = upload(r#"{"schema":"slath-brief/2","intent":"x"}"#).unwrap();
        assert!(wrong.check().is_err());
        let empty = upload(r#"{"schema":"slath-brief/1","intent":"  "}"#).unwrap();
        assert!(empty.check().is_err());
        let ok = upload(r#"{"schema":"slath-brief/1","intent":"x","gaps":""}"#).unwrap();
        assert!(ok.check().is_ok());
        assert_eq!(ok.fields().len(), 1);
        let long = Upload {
            approach: Some("a".repeat(FIELD_MAX_BYTES + 1)),
            ..ok
        };
        assert!(long.check().is_err());
    }
}
