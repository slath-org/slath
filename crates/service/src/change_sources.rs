// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Change sources: branches under the repository's prefix (`push-` by
//! default, as `jj git push --change` names its bookmarks) that carry a
//! merge request and nothing else. Branch lists leave them out.

use slath_core::authz::{Action, Subject};
use slath_core::change_source::check_prefix;
pub use slath_core::change_source::is_source;

use crate::Services;
use crate::error::{Result, ServiceError};
use crate::repos::RepoAccess;

impl Services {
    /// Sets the prefix of the repository's change sources (administrators);
    /// empty turns them off.
    pub async fn set_change_source_prefix(
        &self,
        subject: &Subject,
        a: &RepoAccess,
        prefix: &str,
    ) -> Result<()> {
        self.authorize(subject, a, Action::RepoSettings)?;
        let prefix = prefix.trim();
        check_prefix(prefix)
            .map_err(|m| ServiceError::invalid("change_source_prefix", "invalid", m))?;
        if is_source(prefix, &a.repo.default_branch) {
            return Err(ServiceError::invalid(
                "change_source_prefix",
                "default_branch",
                "The default branch starts with this prefix; choose another.",
            ));
        }
        if prefix == a.repo.change_source_prefix {
            return Ok(());
        }
        slath_db::repos::set_change_source_prefix(&self.pool, a.repo.id, prefix).await?;
        self.audit(
            subject,
            "repo.change_source_prefix",
            crate::audit::Target::repo(*a.repo.id.as_uuid()),
            serde_json::json!({ "prefix": prefix }),
        )
        .await;
        Ok(())
    }
}

/// The title and description of a merge request opened from a commit: its
/// subject line and the rest of its message; the branch name for a commit
/// without a subject.
fn title_and_body(message: &[u8], branch: &str) -> (String, String) {
    let text = String::from_utf8_lossy(message);
    let mut lines = text.lines();
    let title = lines.next().unwrap_or("").trim().to_owned();
    let body = lines.collect::<Vec<_>>().join("\n").trim().to_owned();
    if title.is_empty() {
        (branch.to_owned(), body)
    } else {
        (title, body)
    }
}

impl Services {
    /// Whether a merge request comes from one of its repository's change
    /// sources.
    pub async fn is_change_source(
        &self,
        m: &slath_db::merge_requests::MergeRequest,
    ) -> Result<bool> {
        if m.source_repo_id.is_some_and(|s| s != m.repo_id) {
            return Ok(false);
        }
        let r = slath_db::repos::get(&self.pool, m.repo_id)
            .await?
            .ok_or(ServiceError::NotFound)?;
        Ok(is_source(&r.change_source_prefix, &m.source_branch()))
    }

    /// After a push created or moved the change source `branch` of `r` to
    /// `tip` and no merge request from it is open: opens one into `target`
    /// (the default branch if `None`) as the pusher, titled by the tip
    /// commit. Returns the `remote:` lines saying what happened; a failure
    /// says why and leaves the usual link to create one by hand.
    pub(crate) async fn open_change_source(
        &self,
        subject: &Subject,
        r: &slath_db::repos::Repo,
        branch: &str,
        tip: &str,
        target: Option<&str>,
        create_link: &str,
    ) -> Vec<String> {
        let target = target.unwrap_or(&r.default_branch);
        let failed = |why: &str| {
            vec![
                format!("slath: couldn't open a merge request for {branch}: {why}"),
                "slath: Create one here:".to_owned(),
                format!("slath:   {create_link}"),
            ]
        };
        let dir = self.repo_path(r.id);
        let (Ok(tip), Ok(target_ref)) = (
            slath_git::ObjectId::parse(tip),
            slath_git::args::RefName::branch(target.as_bytes()),
        ) else {
            return failed("the branch names aren't valid");
        };
        let target_head = match self.branch_head(r.id, &target_ref).await {
            Ok(Some(h)) => h,
            Ok(None) => return failed(&format!("the target branch {target} doesn't exist")),
            Err(e) => {
                tracing::warn!(error = %e, "reading the target of a change source failed");
                return failed("try again");
            }
        };
        match slath_git::storage::is_ancestor(&self.runner, &dir, &tip, &target_head, None).await {
            Ok(true) => {
                return vec![format!(
                    "slath: {branch} has no commits that aren't on {target} yet, so no merge request was opened."
                )];
            }
            Ok(false) => {}
            Err(e) => {
                tracing::warn!(error = %e, "comparing a change source with its target failed");
                return failed("try again");
            }
        }
        let message = match slath_git::ObjectReader::open(&self.runner, &dir).await {
            Ok(mut reader) => reader.read_raw_commit(&tip).await.map(|c| c.message),
            Err(e) => Err(e),
        };
        let message = match message {
            Ok(m) => m,
            Err(e) => {
                tracing::warn!(error = %e, "reading the tip of a change source failed");
                return failed("try again");
            }
        };
        if !subject.token_allows(slath_core::authz::Scope::MrsWrite) {
            return failed("the token you pushed with lacks the mrs:write scope");
        }
        let (title, description) = title_and_body(&message, branch);
        let new = crate::merge_requests::NewMr {
            source_branch: branch.to_owned(),
            target_branch: target.to_owned(),
            title,
            description,
            delete_source_on_merge: Some(true),
            ..Default::default()
        };
        match self.create_mr(subject, &r.namespace, &r.name, &new).await {
            Ok(m) => {
                let label = slath_core::change_source::change_id(&r.change_source_prefix, branch)
                    .map_or_else(|| branch.to_owned(), |id| format!("change {}", &id[..8]));
                let url = self.config.server.public_url.join(&format!(
                    "/{}/{}/merge-requests/{}",
                    r.namespace, r.name, m.number
                ));
                vec![
                    format!("slath: Opened !{} for {label}:", m.number),
                    format!("slath:   {url}"),
                ]
            }
            Err(ServiceError::Invalid(errors)) => {
                let why: Vec<&str> = errors.iter().map(|e| e.message.as_str()).collect();
                failed(&why.join(" "))
            }
            Err(e) => {
                tracing::info!(error = %e, "opening a merge request for a change source failed");
                failed("you may not open merge requests here")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::title_and_body;

    #[test]
    fn titles_from_commits() {
        assert_eq!(
            title_and_body(b"Add a\n\nBecause.\n\nChange-Id: I00\n", "push-x"),
            ("Add a".to_owned(), "Because.\n\nChange-Id: I00".to_owned())
        );
        assert_eq!(
            title_and_body(b"\n", "push-x"),
            ("push-x".to_owned(), String::new())
        );
    }
}
