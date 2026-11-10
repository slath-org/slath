// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Repositories: access resolution, creation, settings, rename, archive,
//! deletion (trash), and collaborators.
//!
//! Every lookup goes through [`Services::repo_access`], which loads the
//! subject's effective role and asks the policy. A repository the subject
//! can't see is [`ServiceError::NotFound`], exactly like one that doesn't
//! exist, and a renamed repository's old name only redirects subjects who
//! can read it.

use slath_core::authz::{
    self, Action, Credential, Decision, RepoState, ResourceCtx, Role, Scope, Subject, UserState,
    Visibility,
};
use slath_core::ids::RepoId;
use slath_core::names::RepoName;
use slath_db::orgs::NamespaceKind;
use slath_db::repos::Repo;

use crate::Services;
use crate::error::{FieldError, Result, ServiceError};
use crate::tokens::user_with_scope;

/// A repository and the subject's access to it.
#[derive(Debug, Clone)]
pub struct RepoAccess {
    pub repo: Repo,
    pub ctx: ResourceCtx,
}

impl RepoAccess {
    /// True if `action` would be allowed (for showing or hiding controls).
    #[must_use]
    pub fn can(&self, subject: &Subject, action: Action) -> bool {
        authz::check(subject, action, &self.ctx).is_allowed()
    }
}

/// The result of resolving `namespace/name`.
#[derive(Debug, Clone)]
pub enum Resolved {
    Repo(Box<RepoAccess>),
    /// The name is an old name of a repository the subject can read.
    Moved {
        namespace: String,
        name: String,
    },
}

pub struct NewRepoInput {
    pub namespace: String,
    pub name: String,
    pub description: String,
    pub visibility: Visibility,
    pub default_branch: String,
}

/// Labels every new repository starts with.
const DEFAULT_LABELS: &[(&str, &str, &str)] = &[
    ("bug", "d73a4a", "Something isn't working"),
    ("enhancement", "a2eeef", "A new feature or an improvement"),
    ("documentation", "0075ca", "Changes to the documentation"),
    ("question", "d876e3", "Needs more information"),
    ("good first issue", "7057ff", "Good for newcomers"),
    ("help wanted", "008672", "Extra attention is welcome"),
    ("duplicate", "cfd3d7", "This issue already exists"),
    ("wontfix", "ffffff", "This will not be worked on"),
];

fn invalid_name(e: &slath_core::names::NameError) -> ServiceError {
    let mut f = crate::accounts::name_error(e);
    f.field = "name".to_owned();
    ServiceError::Invalid(vec![f])
}

fn clean_description(d: &str) -> Result<Option<String>> {
    let d = d.trim();
    if d.chars().count() > 500
        || d.chars().any(|c| {
            c.is_control() || matches!(c, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
        })
    {
        return Err(ServiceError::invalid(
            "description",
            "invalid",
            "Use at most 500 characters, without control or text-direction characters.",
        ));
    }
    Ok((!d.is_empty()).then(|| d.to_owned()))
}

fn check_branch(b: &str) -> Result<()> {
    slath_git::args::RefName::branch(b.as_bytes())
        .map(|_| ())
        .map_err(|e| {
            ServiceError::invalid(
                "default_branch",
                "invalid",
                format!("Not a valid branch name: {e}."),
            )
        })
}

/// Browser sessions need sudo mode for the most sensitive actions; tokens
/// are their own credential.
pub(crate) fn require_sudo_for_sessions(subject: &Subject, sudo: bool) -> Result<()> {
    match subject {
        Subject::User {
            credential: Credential::Session,
            ..
        } if !sudo => Err(ServiceError::SudoRequired),
        _ => Ok(()),
    }
}

impl Services {
    /// The subject's effective role on a repository.
    pub async fn effective_role(&self, subject: &Subject, repo: &Repo) -> Result<Option<Role>> {
        let (user, site_admin) = match subject {
            Subject::User {
                id,
                state: UserState::Active,
                is_site_admin,
                ..
            } => (*id, *is_site_admin),
            Subject::System => return Ok(Some(Role::Admin)),
            // A deploy key has its role on its own repository only.
            Subject::DeployKey { repo_id, write, .. } => {
                return Ok((*repo_id == repo.id).then_some(if *write {
                    Role::Write
                } else {
                    Role::Read
                }));
            }
            _ => return Ok(None),
        };
        if site_admin {
            return Ok(Some(Role::Admin));
        }
        let f = slath_db::repos::role_facts(&self.pool, repo.id, user).await?;
        Ok(authz::effective_role(&authz::RoleGrants {
            site_admin: false,
            namespace_owner: f.namespace_owner,
            collaborator: f.collaborator,
            teams: f.team_roles,
            org_owner: f.org_role.as_deref() == Some("owner"),
            org_member: f.org_role.is_some(),
            org_default_member_role: (f.org_default_member_role.as_deref() == Some("read"))
                .then_some(Role::Read),
            org_mfa_unmet: f.mfa_unmet,
        }))
    }

    async fn access_for(&self, subject: &Subject, repo: Repo) -> Result<RepoAccess> {
        let role = self.effective_role(subject, &repo).await?;
        let ctx = ResourceCtx {
            repo_id: repo.id,
            visibility: repo.visibility,
            state: repo.state,
            role,
            allow_forking: repo.allow_forking,
        };
        Ok(RepoAccess { repo, ctx })
    }

    /// Resolves `namespace/name` for the subject, following a rename
    /// redirect only if the subject can read the repository it points to.
    pub async fn resolve_repo(
        &self,
        subject: &Subject,
        namespace: &str,
        name: &str,
    ) -> Result<Resolved> {
        let name = name.strip_suffix(".git").unwrap_or(name);
        if RepoName::parse(name).is_err() || namespace.len() > 64 {
            return Err(ServiceError::NotFound);
        }
        if let Some(repo) = slath_db::repos::find(&self.pool, namespace, name).await? {
            let access = self.access_for(subject, repo).await?;
            return match authz::check(subject, Action::RepoRead, &access.ctx) {
                Decision::Deny { visible: false } => Err(ServiceError::NotFound),
                _ => Ok(Resolved::Repo(Box::new(access))),
            };
        }
        if let Some(id) = slath_db::repos::find_redirect(&self.pool, namespace, name).await?
            && let Some(repo) = slath_db::repos::get(&self.pool, id).await?
        {
            let access = self.access_for(subject, repo).await?;
            if authz::check(subject, Action::RepoRead, &access.ctx).is_allowed() {
                return Ok(Resolved::Moved {
                    namespace: access.repo.namespace,
                    name: access.repo.name,
                });
            }
        }
        Err(ServiceError::NotFound)
    }

    /// Resolves a repository and authorizes `action`: invisible → not found,
    /// visible but not allowed → forbidden, renamed → [`ServiceError::Moved`].
    pub async fn repo_access(
        &self,
        subject: &Subject,
        namespace: &str,
        name: &str,
        action: Action,
    ) -> Result<RepoAccess> {
        match self.resolve_repo(subject, namespace, name).await? {
            Resolved::Moved { namespace, name } => Err(ServiceError::Moved { namespace, name }),
            Resolved::Repo(access) => match authz::check(subject, action, &access.ctx) {
                Decision::Allow => Ok(*access),
                Decision::Deny { visible: false } => Err(ServiceError::NotFound),
                Decision::Deny { visible: true } => Err(ServiceError::Forbidden),
            },
        }
    }

    /// Like [`Services::repo_access`] by ID (jobs, IPC).
    pub async fn repo_access_by_id(
        &self,
        subject: &Subject,
        id: RepoId,
        action: Action,
    ) -> Result<RepoAccess> {
        let repo = slath_db::repos::get(&self.pool, id)
            .await?
            .ok_or(ServiceError::NotFound)?;
        let access = self.access_for(subject, repo).await?;
        match authz::check(subject, action, &access.ctx) {
            Decision::Allow => Ok(access),
            Decision::Deny { visible: false } => Err(ServiceError::NotFound),
            Decision::Deny { visible: true } => Err(ServiceError::Forbidden),
        }
    }

    /// Namespaces the subject can create repositories in: their own, the
    /// organizations they belong to, and (for site administrators) any.
    pub async fn can_create_in(
        &self,
        subject: &Subject,
        namespace: &str,
    ) -> Result<Option<slath_core::ids::NamespaceId>> {
        let Subject::User {
            id,
            state: UserState::Active,
            is_site_admin,
            ..
        } = subject
        else {
            return Ok(None);
        };
        let Some((ns_id, _, kind)) =
            slath_db::orgs::resolve_namespace(&self.pool, namespace).await?
        else {
            return Ok(None);
        };
        let allowed = *is_site_admin
            || match kind {
                NamespaceKind::User(u) => u == *id,
                NamespaceKind::Org(o) => {
                    slath_db::orgs::role_of(&self.pool, o, *id).await?.is_some()
                }
            };
        Ok(allowed.then_some(ns_id))
    }

    /// Creates a repository. The bare repository is initialized in a
    /// temporary directory and renamed into place, inside the database
    /// transaction that creates the row: a crash leaves either nothing or a
    /// complete repository, never a half-initialized one.
    pub async fn create_repo(&self, subject: &Subject, input: NewRepoInput) -> Result<RepoAccess> {
        self.create_repo_as(subject, input, None).await
    }

    /// [`Self::create_repo`], and for a fork the record and job that copy
    /// its parent, in the same transaction.
    pub(crate) async fn create_repo_as(
        &self,
        subject: &Subject,
        input: NewRepoInput,
        fork: Option<&crate::forks::ForkPlan>,
    ) -> Result<RepoAccess> {
        user_with_scope(subject, Scope::RepoWrite)?;
        self.require_confirmed(subject).await?;
        let mut errors: Vec<FieldError> = Vec::new();
        let name = match RepoName::parse(input.name.trim()) {
            Ok(n) => Some(n),
            Err(e) => {
                if let ServiceError::Invalid(mut f) = invalid_name(&e) {
                    errors.append(&mut f);
                }
                None
            }
        };
        let description = match clean_description(&input.description) {
            Ok(d) => d,
            Err(ServiceError::Invalid(mut f)) => {
                errors.append(&mut f);
                None
            }
            Err(e) => return Err(e),
        };
        let branch = input.default_branch.trim();
        let branch = if branch.is_empty() { "main" } else { branch };
        if let Err(ServiceError::Invalid(mut f)) = check_branch(branch) {
            errors.append(&mut f);
        }
        let ns_id = self.can_create_in(subject, &input.namespace).await?;
        if ns_id.is_none() {
            errors.push(FieldError::new(
                "namespace",
                "not_allowed",
                "You can't create repositories there.",
            ));
        }
        let (Some(name), Some(ns_id)) = (name, ns_id) else {
            return Err(ServiceError::Invalid(errors));
        };
        if !errors.is_empty() {
            return Err(ServiceError::Invalid(errors));
        }
        let id = RepoId::new();
        let mut tx = self.pool.begin().await.map_err(ServiceError::internal)?;
        slath_db::repos::insert(
            &mut *tx,
            &slath_db::repos::NewRepo {
                id,
                namespace_id: ns_id,
                name: name.as_str(),
                description: description.as_deref(),
                visibility: input.visibility,
                default_branch: branch,
            },
        )
        .await
        .map_err(|e| match e {
            slath_db::DbError::Conflict(_) => ServiceError::invalid(
                "name",
                "taken",
                "A repository with this name already exists.",
            ),
            other => other.into(),
        })?;
        // The creator administers a repository they create in an organization,
        // whatever their default access there.
        if let Subject::User { id: creator, .. } = subject
            && slath_db::orgs::resolve_namespace(&mut *tx, &input.namespace)
                .await?
                .is_some_and(|(_, _, k)| matches!(k, NamespaceKind::Org(_)))
        {
            slath_db::repos::set_collaborator(&mut *tx, id, *creator, Role::Admin).await?;
        }
        // A starting set of labels, which maintainers can change or delete.
        for (name, color, description) in DEFAULT_LABELS {
            slath_db::labels::create_label(&mut *tx, id, name, color, Some(description)).await?;
        }
        // Creators watch what they create.
        if let Subject::User { id: creator, .. } = subject {
            slath_db::notifications::set_subscription(
                &mut *tx,
                *creator,
                "repo",
                *id.as_uuid(),
                id,
                Some("watching"),
            )
            .await?;
        }
        if let Some(f) = fork {
            slath_db::forks::insert(
                &mut tx,
                id,
                f.parent,
                f.default_branch_only,
                f.max_visibility,
                f.by,
            )
            .await?;
            slath_db::jobs::enqueue(
                &mut *tx,
                &slath_db::jobs::NewJob::new(
                    "default",
                    crate::forks::REPO_FORK,
                    serde_json::json!({"repo_id": id.to_string()}),
                )
                .dedupe_key(format!("{}:{id}", crate::forks::REPO_FORK)),
            )
            .await?;
        }
        let path = slath_git::storage::create_bare(
            &self.runner,
            &self.config.storage.data_dir,
            id,
            branch,
        )
        .await
        .map_err(ServiceError::internal)?;
        if let Err(e) = tx.commit().await {
            let _ = tokio::fs::remove_dir_all(&path).await;
            return Err(ServiceError::internal(e));
        }
        let a = self
            .repo_access_by_id(subject, id, Action::RepoRead)
            .await?;
        let target = self.repo_target(&a.repo).await;
        self.audit(subject, "repo.create", target, serde_json::json!({"name": format!("{}/{}", a.repo.namespace, a.repo.name), "visibility": slath_db::repos::visibility_str(input.visibility)})).await;
        Ok(a)
    }

    pub async fn update_repo_settings(
        &self,
        subject: &Subject,
        a: &RepoAccess,
        description: &str,
        default_branch: &str,
    ) -> Result<()> {
        self.authorize(subject, a, Action::RepoSettings)?;
        let description = clean_description(description)?;
        let branch = default_branch.trim();
        check_branch(branch)?;
        slath_db::repos::update_settings(&self.pool, a.repo.id, description.as_deref(), branch)
            .await?;
        let head = format!("refs/heads/{branch}");
        slath_git::storage::set_head(&self.runner, &self.repo_path(a.repo.id), &head)
            .await
            .map_err(ServiceError::internal)?;
        Ok(())
    }

    /// Changes visibility (sudo mode for browsers).
    pub async fn set_repo_visibility(
        &self,
        subject: &Subject,
        sudo: bool,
        a: &RepoAccess,
        v: Visibility,
    ) -> Result<()> {
        self.authorize(subject, a, Action::RepoVisibility)?;
        require_sudo_for_sessions(subject, sudo)?;
        if let Some(f) = slath_db::forks::get(&self.pool, a.repo.id).await?
            && crate::forks::rank(v) > crate::forks::rank(f.max_visibility)
        {
            return Err(ServiceError::invalid(
                "visibility",
                "fork",
                format!(
                    "A fork of a {} repository can't be more visible than that.",
                    slath_db::repos::visibility_str(f.max_visibility)
                ),
            ));
        }
        slath_db::repos::set_visibility(&self.pool, a.repo.id, v).await?;
        let target = self.repo_target(&a.repo).await;
        self.audit(
            subject,
            "repo.visibility",
            target,
            serde_json::json!({"visibility": slath_db::repos::visibility_str(v)}),
        )
        .await;
        self.review_forks(a.repo.id).await;
        Ok(())
    }

    /// Renames within the namespace; the old name keeps redirecting readers.
    pub async fn rename_repo(
        &self,
        subject: &Subject,
        a: &RepoAccess,
        new_name: &str,
    ) -> Result<String> {
        self.authorize(subject, a, Action::RepoRename)?;
        let name = RepoName::parse(new_name.trim()).map_err(|e| invalid_name(&e))?;
        let mut tx = self.pool.begin().await.map_err(ServiceError::internal)?;
        if !name.as_str().eq_ignore_ascii_case(&a.repo.name)
            && slath_db::repos::name_in_use(&mut *tx, a.repo.namespace_id, name.as_str()).await?
        {
            return Err(ServiceError::invalid(
                "name",
                "taken",
                "A repository with this name already exists.",
            ));
        }
        slath_db::repos::rename(&mut tx, a.repo.id, name.as_str())
            .await
            .map_err(|e| match e {
                slath_db::DbError::Conflict(_) => ServiceError::invalid(
                    "name",
                    "taken",
                    "A repository with this name already exists.",
                ),
                other => other.into(),
            })?;
        tx.commit().await.map_err(ServiceError::internal)?;
        let target = self.repo_target(&a.repo).await;
        self.audit(
            subject,
            "repo.rename",
            target,
            serde_json::json!({"from": a.repo.name, "to": name.as_str()}),
        )
        .await;
        Ok(name.as_str().to_owned())
    }

    pub async fn set_repo_archived(
        &self,
        subject: &Subject,
        a: &RepoAccess,
        archived: bool,
    ) -> Result<()> {
        self.authorize(subject, a, Action::RepoArchive)?;
        let state = if archived {
            RepoState::Archived
        } else {
            RepoState::Active
        };
        slath_db::repos::set_state(&self.pool, a.repo.id, state).await?;
        let target = self.repo_target(&a.repo).await;
        self.audit(
            subject,
            if archived {
                "repo.archive"
            } else {
                "repo.unarchive"
            },
            target,
            serde_json::json!({}),
        )
        .await;
        Ok(())
    }

    /// Deletes a repository (sudo mode for browsers): it becomes invisible
    /// immediately, a job moves it to the trash, and it's purged after
    /// `maintenance.deleted_repo_days`.
    pub async fn delete_repo(&self, subject: &Subject, sudo: bool, a: &RepoAccess) -> Result<()> {
        self.authorize(subject, a, Action::RepoDelete)?;
        require_sudo_for_sessions(subject, sudo)?;
        let mut tx = self.pool.begin().await.map_err(ServiceError::internal)?;
        slath_db::repos::set_state(&mut *tx, a.repo.id, RepoState::Deleting).await?;
        slath_db::jobs::enqueue(
            &mut *tx,
            &slath_db::jobs::NewJob::new(
                "default",
                crate::jobs::REPO_TRASH,
                serde_json::json!({"repo_id": a.repo.id.to_string()}),
            )
            .dedupe_key(format!("repo.trash:{}", a.repo.id)),
        )
        .await?;
        tx.commit().await.map_err(ServiceError::internal)?;
        let target = self.repo_target(&a.repo).await;
        self.audit(
            subject,
            "repo.delete",
            target,
            serde_json::json!({"name": format!("{}/{}", a.repo.namespace, a.repo.name)}),
        )
        .await;
        Ok(())
    }

    /// Restores a repository from the trash (site administrators).
    pub async fn restore_repo(&self, subject: &Subject, id: RepoId) -> Result<()> {
        if !matches!(
            subject,
            Subject::User {
                is_site_admin: true,
                state: UserState::Active,
                ..
            } | Subject::System
        ) {
            return Err(ServiceError::NotFound);
        }
        let repo = slath_db::repos::get(&self.pool, id)
            .await?
            .ok_or(ServiceError::NotFound)?;
        if repo.state != RepoState::Deleting {
            return Err(ServiceError::NotFound);
        }
        if slath_db::repos::find(&self.pool, &repo.namespace, &repo.name)
            .await?
            .is_some_and(|r| r.id != id && r.state != RepoState::Deleting)
        {
            return Err(ServiceError::Conflict(
                "Another repository now uses this name.".to_owned(),
            ));
        }
        let live = self.repo_path(id);
        let trash = self.trash_path(id);
        if !live.exists() && trash.exists() {
            if let Some(p) = live.parent() {
                tokio::fs::create_dir_all(p)
                    .await
                    .map_err(ServiceError::internal)?;
            }
            tokio::fs::rename(&trash, &live)
                .await
                .map_err(ServiceError::internal)?;
        }
        if !live.exists() {
            return Err(ServiceError::Conflict(
                "The repository's data is gone.".to_owned(),
            ));
        }
        slath_db::repos::set_state(&self.pool, id, RepoState::Active).await?;
        let target = self.repo_target(&repo).await;
        self.audit(
            subject,
            "repo.restore",
            target,
            serde_json::json!({"name": format!("{}/{}", repo.namespace, repo.name)}),
        )
        .await;
        Ok(())
    }

    pub async fn list_collaborators(
        &self,
        subject: &Subject,
        a: &RepoAccess,
    ) -> Result<Vec<slath_db::repos::Collaborator>> {
        self.authorize(subject, a, Action::CollaboratorsManage)?;
        Ok(slath_db::repos::collaborators(&self.pool, a.repo.id).await?)
    }

    pub async fn set_collaborator(
        &self,
        subject: &Subject,
        a: &RepoAccess,
        username: &str,
        role: Role,
    ) -> Result<()> {
        self.authorize(subject, a, Action::CollaboratorsManage)?;
        let user = match slath_db::orgs::resolve_namespace(&self.pool, username.trim()).await? {
            Some((_, _, NamespaceKind::User(u))) => u,
            _ => {
                return Err(ServiceError::invalid(
                    "username",
                    "unknown",
                    "There's no user with this name.",
                ));
            }
        };
        slath_db::repos::set_collaborator(&self.pool, a.repo.id, user, role).await?;
        let target = self.repo_target(&a.repo).await;
        self.audit(subject, "repo.collaborator.set", target, serde_json::json!({"username": username.trim(), "role": slath_db::repos::role_str(role)})).await;
        self.review_forks(a.repo.id).await;
        Ok(())
    }

    pub async fn remove_collaborator(
        &self,
        subject: &Subject,
        a: &RepoAccess,
        username: &str,
    ) -> Result<()> {
        self.authorize(subject, a, Action::CollaboratorsManage)?;
        let Some((_, _, NamespaceKind::User(user))) =
            slath_db::orgs::resolve_namespace(&self.pool, username.trim()).await?
        else {
            return Err(ServiceError::NotFound);
        };
        if slath_db::repos::remove_collaborator(&self.pool, a.repo.id, user).await? {
            let target = self.repo_target(&a.repo).await;
            self.audit(
                subject,
                "repo.collaborator.remove",
                target,
                serde_json::json!({"username": username.trim()}),
            )
            .await;
            self.review_forks(a.repo.id).await;
            Ok(())
        } else {
            Err(ServiceError::NotFound)
        }
    }

    /// Repositories of a namespace the subject can see.
    pub async fn list_namespace_repos(
        &self,
        subject: &Subject,
        namespace: slath_core::ids::NamespaceId,
    ) -> Result<Vec<Repo>> {
        let (viewer, admin) = match subject {
            Subject::User {
                id,
                state: UserState::Active,
                is_site_admin,
                credential,
                ..
            } => {
                // A repository-restricted token sees only public repositories in lists.
                let restricted = matches!(
                    credential,
                    Credential::Token {
                        repo_ids: Some(_),
                        ..
                    }
                );
                (
                    if restricted { None } else { Some(*id) },
                    *is_site_admin && !restricted,
                )
            }
            _ => (None, false),
        };
        let mut repos =
            slath_db::repos::list_visible_in_namespace(&self.pool, namespace, viewer, admin, 500)
                .await?;
        if let Subject::User {
            credential:
                Credential::Token {
                    repo_ids: Some(ids),
                    ..
                },
            ..
        } = subject
        {
            repos.retain(|r| r.visibility == Visibility::Public || ids.contains(&r.id));
        }
        Ok(repos)
    }

    /// The audit target for a repository (with its organization, if any).
    pub(crate) async fn repo_target(&self, repo: &Repo) -> crate::audit::Target {
        let mut t = crate::audit::Target::repo(*repo.id.as_uuid());
        if repo.namespace_kind == "org"
            && let Ok(Some((_, _, NamespaceKind::Org(o)))) =
                slath_db::orgs::resolve_namespace(&self.pool, &repo.namespace).await
        {
            t.org_id = Some(*o.as_uuid());
        }
        t
    }

    /// Re-checks an action on an already resolved repository.
    pub fn authorize(&self, subject: &Subject, a: &RepoAccess, action: Action) -> Result<()> {
        match authz::check(subject, action, &a.ctx) {
            Decision::Allow => Ok(()),
            Decision::Deny { visible: false } => Err(ServiceError::NotFound),
            Decision::Deny { visible: true } => Err(ServiceError::Forbidden),
        }
    }

    /// The on-disk path of a live repository.
    #[must_use]
    pub fn repo_path(&self, id: RepoId) -> std::path::PathBuf {
        slath_git::storage::repo_path(&self.config.storage.data_dir, id)
    }

    /// Where a deleted repository waits for its purge.
    #[must_use]
    pub fn trash_path(&self, id: RepoId) -> std::path::PathBuf {
        self.config
            .storage
            .data_dir
            .join("trash")
            .join(format!("{id}.git"))
    }
}

impl Services {
    /// Repositories the subject can see in any namespace, by full name.
    pub async fn list_visible_repos(
        &self,
        subject: &Subject,
        offset: usize,
        limit: usize,
    ) -> Result<Vec<Repo>> {
        let (viewer, admin, restricted) = match subject {
            Subject::User {
                id,
                state: UserState::Active,
                is_site_admin,
                credential,
                ..
            } => match credential {
                // Without `repo:read`, a token sees what anonymous visitors see.
                Credential::Token { scopes, .. }
                    if !scopes.iter().any(|s| s.implies(Scope::RepoRead)) =>
                {
                    (None, false, None)
                }
                Credential::Token {
                    repo_ids: Some(ids),
                    ..
                } => (None, false, Some(ids.clone())),
                _ => (Some(*id), *is_site_admin, None),
            },
            _ => (None, false, None),
        };
        let offset = i64::try_from(offset).unwrap_or(i64::MAX);
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        let mut repos = match &restricted {
            // A repository-restricted token sees its repositories plus public ones.
            Some(ids) => {
                let owner = match subject {
                    Subject::User { id, .. } => Some(*id),
                    _ => None,
                };
                let mut all =
                    slath_db::repos::list_visible(&self.pool, owner, false, 0, 10_000).await?;
                all.retain(|r| r.visibility == Visibility::Public || ids.contains(&r.id));
                all.into_iter()
                    .skip(usize::try_from(offset).unwrap_or(0))
                    .take(usize::try_from(limit).unwrap_or(0))
                    .collect()
            }
            None => slath_db::repos::list_visible(&self.pool, viewer, admin, offset, limit).await?,
        };
        repos.retain(|r| r.state != RepoState::Deleting);
        Ok(repos)
    }
}
