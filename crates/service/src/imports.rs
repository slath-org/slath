// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Importing a repository (branches and tags) from another server.
//!
//! The request creates an empty repository and queues the `repo.import`
//! job; the URL is checked when it's entered and again, with a fresh name
//! resolution, when the job runs, and Git connects only to the address
//! that was checked. Credentials travel encrypted in the job, are written
//! to a file only the server can read for the duration of the fetch, and
//! are removed from the job afterwards. The result (branches, tags, size,
//! or the reason it failed) is kept as the import report.

use slath_core::Secret;
use slath_core::authz::{Action, Subject, Visibility};
use slath_core::ids::RepoId;
use slath_db::imports::Import;
use slath_db::jobs::LeasedJob;
use slath_db::worker::JobError;

use crate::Services;
use crate::error::{Result, ServiceError};
use crate::net::{self, UrlError};
use crate::repos::{NewRepoInput, RepoAccess};

/// The job that fetches an imported repository.
pub const REPO_IMPORT: &str = "repo.import";

/// What to import, and where to.
#[derive(Debug)]
pub struct ImportInput {
    pub namespace: String,
    pub name: String,
    pub description: String,
    pub visibility: Visibility,
    pub url: String,
    pub username: String,
    pub password: Secret<String>,
}

fn url_error(e: &UrlError) -> ServiceError {
    ServiceError::invalid("url", "invalid_url", e.to_string())
}

#[derive(serde::Serialize, serde::Deserialize)]
struct Credentials {
    username: String,
    password: String,
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::new(), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

/// A short, safe message from Git's error output for the import report.
fn git_message(stderr: &[u8]) -> String {
    let text = String::from_utf8_lossy(stderr);
    let line = text
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("");
    let clean: String = line
        .trim_start_matches("fatal: ")
        .trim_start_matches("error: ")
        .chars()
        .filter(|c| !c.is_control())
        .take(300)
        .collect();
    if clean.is_empty() {
        "Git couldn't fetch the repository.".to_owned()
    } else {
        format!("Git couldn't fetch the repository: {clean}")
    }
}

impl Services {
    fn own_host(&self) -> String {
        self.config.server.public_url.host().to_owned()
    }

    /// Creates a repository and queues the import into it.
    pub async fn import_repo(&self, subject: &Subject, input: ImportInput) -> Result<RepoAccess> {
        let user = match subject {
            Subject::User { id, .. } => *id,
            _ => return Err(ServiceError::Unauthenticated),
        };
        if !self.config.import.enabled {
            return Err(ServiceError::NotFound);
        }
        // Permissions before anything about the URL.
        crate::tokens::user_with_scope(subject, slath_core::authz::Scope::RepoWrite)?;
        self.require_confirmed(subject).await?;
        let url = net::parse(input.url.trim(), self.config.import.allow_http)
            .map_err(|e| url_error(&e))?;
        // Checked now for a quick answer, and again when the job runs.
        net::resolve(&url, &self.config.import.allow_networks, &self.own_host())
            .await
            .map_err(|e| url_error(&e))?;
        let a = self
            .create_repo(
                subject,
                NewRepoInput {
                    namespace: input.namespace,
                    name: input.name,
                    description: input.description,
                    visibility: input.visibility,
                    default_branch: "main".to_owned(),
                },
            )
            .await?;
        slath_db::imports::insert(&self.pool, a.repo.id, url.as_str(), user).await?;
        let job_id = uuid::Uuid::now_v7().to_string();
        let mut payload = serde_json::json!({
            "repo_id": a.repo.id.to_string(),
            "job": job_id,
        });
        let password = input.password.expose();
        if !input.username.is_empty() || !password.is_empty() {
            let creds = serde_json::to_vec(&Credentials {
                username: input.username.clone(),
                password: password.clone(),
            })
            .map_err(ServiceError::internal)?;
            let sealed = self.seal(&creds, "jobs", "credentials", &job_id).await?;
            payload["key_id"] = sealed.key_id.clone().into();
            payload["credentials"] = hex(&sealed.ciphertext).into();
        }
        slath_db::jobs::enqueue(
            &self.pool,
            &slath_db::jobs::NewJob::new("default", REPO_IMPORT, payload)
                .dedupe_key(format!("{REPO_IMPORT}:{}", a.repo.id)),
        )
        .await?;
        let target = self.repo_target(&a.repo).await;
        self.audit(
            subject,
            "repo.import",
            target,
            serde_json::json!({"url": url.as_str()}),
        )
        .await;
        Ok(a)
    }

    /// The import report of a repository the subject can read, if it was
    /// imported.
    pub async fn import_report(&self, subject: &Subject, a: &RepoAccess) -> Result<Option<Import>> {
        self.authorize(subject, a, Action::RepoRead)?;
        Ok(slath_db::imports::get(&self.pool, a.repo.id).await?)
    }

    /// The `repo.import` job.
    pub async fn run_import(&self, job: &LeasedJob) -> std::result::Result<(), JobError> {
        let id: RepoId = job
            .payload
            .get("repo_id")
            .and_then(serde_json::Value::as_str)
            .and_then(|s| s.parse().ok())
            .ok_or_else(|| JobError::Permanent("payload has no valid repo_id".to_owned()))?;
        let result = self.import_once(job, id).await;
        // Credentials are used once.
        let _ = slath_db::imports::forget_job_credentials(&self.pool, job.id).await;
        let retry = |e: slath_db::DbError| JobError::Retry(e.to_string());
        match result {
            Ok(Some((branches, tags, size))) => {
                // Report-only: imported history isn't rejected. Scanned
                // before the import counts as done, so the report is whole.
                if let Err(e) = self.scan_import(id).await {
                    tracing::warn!(error = %e, repo = %id, "scanning the imported history failed");
                }
                slath_db::imports::finish(&self.pool, id, branches, tags, size)
                    .await
                    .map_err(retry)?;
                Ok(())
            }
            Ok(None) => Ok(()),
            Err(message) => {
                slath_db::imports::fail(&self.pool, id, &message)
                    .await
                    .map_err(retry)?;
                Ok(())
            }
        }
    }

    /// Fetches once. `Ok(None)` if there is nothing (left) to do; `Err`
    /// carries the message for the report.
    async fn import_once(
        &self,
        job: &LeasedJob,
        id: RepoId,
    ) -> std::result::Result<Option<(i32, i32, i64)>, String> {
        let internal = |e: &dyn std::fmt::Display| {
            tracing::error!(error = %e, repo = %id, "import failed");
            "The import failed because of an internal error.".to_owned()
        };
        let Some(import) = slath_db::imports::get(&self.pool, id)
            .await
            .map_err(|e| internal(&e))?
        else {
            return Ok(None);
        };
        if import.state == "done" {
            return Ok(None);
        }
        let Some(repo) = slath_db::repos::get(&self.pool, id)
            .await
            .map_err(|e| internal(&e))?
        else {
            return Ok(None);
        };
        slath_db::imports::set_running(&self.pool, id)
            .await
            .map_err(|e| internal(&e))?;
        let url =
            net::parse(&import.url, self.config.import.allow_http).map_err(|e| e.to_string())?;
        let dest = net::resolve(&url, &self.config.import.allow_networks, &self.own_host())
            .await
            .map_err(|e| e.to_string())?;

        // Credentials go into a private config file for this fetch only.
        let creds_file = match (
            job.payload
                .get("credentials")
                .and_then(serde_json::Value::as_str),
            job.payload
                .get("key_id")
                .and_then(serde_json::Value::as_str),
            job.payload.get("job").and_then(serde_json::Value::as_str),
        ) {
            (Some(c), Some(key), Some(job_id)) => {
                let bytes: Vec<u8> = (0..c.len() / 2)
                    .filter_map(|i| u8::from_str_radix(c.get(2 * i..2 * i + 2)?, 16).ok())
                    .collect();
                let plain = self
                    .open(&bytes, key, "jobs", "credentials", job_id)
                    .map_err(|e| internal(&e))?;
                let creds: Credentials =
                    serde_json::from_slice(plain.expose()).map_err(|e| internal(&e))?;
                let basic = {
                    use base64::Engine;
                    base64::engine::general_purpose::STANDARD
                        .encode(format!("{}:{}", creds.username, creds.password))
                };
                let dir = self.config.storage.data_dir.join("tmp");
                std::fs::create_dir_all(&dir).map_err(|e| internal(&e))?;
                let path = dir.join(format!("import-{}.gitconfig", uuid::Uuid::now_v7()));
                write_private(
                    &path,
                    &format!("[http]\n\textraHeader = Authorization: Basic {basic}\n"),
                )
                .map_err(|e| internal(&e))?;
                Some(TempFile(path))
            }
            _ => None,
        };

        let git_dir = self.repo_path(id);
        let remote = slath_git::import::Remote {
            url: dest.url.as_str(),
            host: dest.host.trim_start_matches('[').trim_end_matches(']'),
            port: dest.port,
            ip: dest.ip,
            http: dest.url.scheme() == "http",
            credentials: creds_file.as_ref().map(|f| f.0.as_path()),
        };
        let max = self.config.limits.push_max_bytes.0;
        let fetched = slath_git::import::fetch(
            &self.runner,
            &git_dir,
            &remote,
            self.config.limits.import_timeout.get(),
            max,
        )
        .await;
        drop(creds_file);
        if let Err(e) = fetched {
            return Err(match e {
                slath_git::GitError::Limit(slath_git::error::LimitKind::Timeout) => format!(
                    "The import took longer than {} seconds.",
                    self.config.limits.import_timeout.get().as_secs()
                ),
                slath_git::GitError::Limit(_) => format!(
                    "The repository is larger than the limit of {} MiB.",
                    max / (1024 * 1024)
                ),
                slath_git::GitError::Failed { stderr, .. } => git_message(&stderr),
                other => internal(&other),
            });
        }

        // The default branch: keep it if it came along, else main, master,
        // or the first branch.
        let list =
            |kind| slath_git::browse::list_refs(&self.runner, &git_dir, kind, i32::MAX as usize);
        let branches: Vec<String> = list(slath_git::browse::RefKind::Branch)
            .await
            .map_err(|e| internal(&e))?
            .into_iter()
            .map(|r| String::from_utf8_lossy(&r.name).into_owned())
            .collect();
        let tags = list(slath_git::browse::RefKind::Tag)
            .await
            .map_err(|e| internal(&e))?
            .len();
        if !branches.contains(&repo.default_branch) {
            let pick = ["main", "master"]
                .iter()
                .find(|c| branches.iter().any(|b| b == *c))
                .map(|c| (*c).to_owned())
                .or_else(|| branches.first().cloned());
            if let Some(b) = pick {
                slath_db::repos::update_settings(&self.pool, id, repo.description.as_deref(), &b)
                    .await
                    .map_err(|e| internal(&e))?;
                slath_git::storage::set_head(&self.runner, &git_dir, &format!("refs/heads/{b}"))
                    .await
                    .map_err(|e| internal(&e))?;
            }
        }
        // The database's view of the refs follows Git.
        self.reconcile_repo(id, true)
            .await
            .map_err(|e| internal(&e))?;
        let size = tokio::task::spawn_blocking({
            let objects = git_dir.join("objects");
            move || dir_size(&objects)
        })
        .await
        .unwrap_or(0);
        Ok(Some((
            i32::try_from(branches.len()).unwrap_or(i32::MAX),
            i32::try_from(tags).unwrap_or(i32::MAX),
            i64::try_from(size).unwrap_or(i64::MAX),
        )))
    }
}

/// Deletes the file when dropped.
struct TempFile(std::path::PathBuf);

impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn write_private(path: &std::path::Path, text: &str) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    f.write_all(text.as_bytes())?;
    f.sync_all()
}

fn dir_size(path: &std::path::Path) -> u64 {
    let mut total = 0;
    let mut stack = vec![path.to_path_buf()];
    while let Some(p) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&p) else {
            continue;
        };
        for e in rd.flatten() {
            match e.metadata() {
                Ok(m) if m.is_dir() => stack.push(e.path()),
                Ok(m) => total += m.len(),
                Err(_) => {}
            }
        }
    }
    total
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn git_messages_are_short_and_clean() {
        assert_eq!(
            git_message(b"fatal: repository 'https://x/y' not found\n"),
            "Git couldn't fetch the repository: repository 'https://x/y' not found"
        );
        assert_eq!(git_message(b""), "Git couldn't fetch the repository.");
        assert!(git_message(&[b'a'; 1000]).len() < 400);
    }
}
