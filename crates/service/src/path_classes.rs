// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Path classes: the built-in ones and those of `.slath/paths.toml` on the
//! default branch, read on every use (nothing is stored), and the view that
//! shows what's in effect, what's wrong with the file, and which classes
//! the tree suggests.

use slath_core::authz::{Action, Subject};
use slath_core::ids::RepoId;
use slath_core::path_classes::{Class, ClassError, FILE, PathClasses, Proposal, proposals};
use slath_git::args::RefName;

use crate::Services;
use crate::error::{Result, ServiceError};

/// One class in effect and how many files of the default branch it has.
#[derive(Debug, Clone)]
pub struct ClassView {
    pub class: Class,
    pub files: usize,
}

/// What the path classes page shows.
#[derive(Debug, Clone)]
pub struct PathClassesView {
    pub classes: Vec<ClassView>,
    pub errors: Vec<ClassError>,
    pub proposals: Vec<Proposal>,
    /// Whether the default branch has the file.
    pub has_file: bool,
    /// Whether the tree had more files than were counted.
    pub files_cut: bool,
}

impl Services {
    /// The path classes in effect for `repo`: the built-in ones and those
    /// of `.slath/paths.toml` as it stands on the default branch (never
    /// from a merge request's commits, so a change can't reclassify
    /// itself).
    pub(crate) async fn path_classes(&self, repo: RepoId, default_branch: &str) -> PathClasses {
        let blob = self
            .blob_at(repo, &format!("refs/heads/{default_branch}"), FILE)
            .await;
        let text = blob.as_deref().map(String::from_utf8_lossy);
        PathClasses::parse(text.as_deref())
    }

    /// The path classes page of a repository, for whoever can read it.
    pub async fn repo_path_classes(
        &self,
        subject: &Subject,
        ns: &str,
        repo: &str,
    ) -> Result<PathClassesView> {
        let a = self
            .repo_access(subject, ns, repo, Action::RepoRead)
            .await?;
        let dir = self.repo_path(a.repo.id);
        let branch = RefName::parse(format!("refs/heads/{}", a.repo.default_branch).as_bytes())
            .map_err(ServiceError::internal)?;
        let head = slath_git::refs::read(&self.runner, &dir, &branch)
            .await
            .map_err(ServiceError::internal)?;
        let Some(head) = head else {
            // An empty repository: only the built-in classes, no files.
            let classes = PathClasses::parse(None);
            return Ok(PathClassesView {
                classes: classes
                    .classes()
                    .iter()
                    .map(|c| ClassView {
                        class: c.clone(),
                        files: 0,
                    })
                    .collect(),
                errors: Vec::new(),
                proposals: Vec::new(),
                has_file: false,
                files_cut: false,
            });
        };
        // The file and the tree from the same commit.
        let blob = self.blob_at(a.repo.id, head.as_str(), FILE).await;
        let has_file = blob.is_some();
        let text = blob.as_deref().map(String::from_utf8_lossy);
        let classes = PathClasses::parse(text.as_deref());
        let mut paths = slath_git::browse::changed_paths(&self.runner, &dir, None, &head)
            .await
            .map_err(ServiceError::internal)?;
        let max = self.config.limits.path_classes_tree_max_files;
        let files_cut = paths.len() > max;
        paths.truncate(max);
        let views = classes
            .classes()
            .iter()
            .map(|c| ClassView {
                class: c.clone(),
                files: paths.iter().filter(|p| c.matches(p)).count(),
            })
            .collect();
        Ok(PathClassesView {
            classes: views,
            errors: classes.errors().to_vec(),
            proposals: proposals(paths.iter().map(Vec::as_slice), &classes),
            has_file,
            files_cut,
        })
    }
}
