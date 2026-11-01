// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Labels, milestones, and assignees: managing labels and milestones
//! (`labels.manage`, `milestones.manage`: maintain), applying them to
//! issues (`issue.triage`: write), and resolving list filters.

use std::collections::{BTreeSet, HashMap};

use slath_core::authz::{Action, Subject};
use slath_core::ids::UserId;
use slath_db::issues::Filter;
use slath_db::labels::{Label, Milestone};

use crate::Services;
use crate::error::{FieldError, Result, ServiceError};
use crate::repos::RepoAccess;

/// Assignees per issue.
pub const MAX_ASSIGNEES: usize = 10;

/// Labels and assignees of a page of issues.
#[derive(Debug, Clone, Default)]
pub struct IssueMeta {
    pub labels: HashMap<uuid::Uuid, Vec<Label>>,
    pub assignees: HashMap<uuid::Uuid, Vec<String>>,
}

/// List filters as people type them (names, not IDs).
#[derive(Debug, Clone, Default)]
pub struct FilterInput {
    pub state: Option<String>,
    pub labels: Vec<String>,
    pub assignee: Option<String>,
    pub author: Option<String>,
    pub milestone: Option<String>,
    pub sort_updated: bool,
}

fn clean(field: &str, s: &str, max: usize, what: &str) -> Result<String> {
    let t = s.trim();
    if t.is_empty() {
        return Err(ServiceError::invalid(
            field,
            "required",
            format!("Enter a {what}."),
        ));
    }
    if t.chars().count() > max || t.chars().any(|c| c.is_control()) {
        return Err(ServiceError::invalid(
            field,
            "invalid",
            format!("Use at most {max} characters, without control characters."),
        ));
    }
    Ok(t.to_owned())
}

fn optional(field: &str, s: &str, max: usize) -> Result<Option<String>> {
    let t = s.trim();
    if t.chars().count() > max || t.chars().any(|c| c.is_control() && c != '\n') {
        return Err(ServiceError::invalid(
            field,
            "invalid",
            format!("Use at most {max} characters."),
        ));
    }
    Ok((!t.is_empty()).then(|| t.to_owned()))
}

fn color(s: &str) -> Result<String> {
    let c = s.trim().trim_start_matches('#').to_ascii_lowercase();
    if c.len() == 6 && c.bytes().all(|b| b.is_ascii_hexdigit()) {
        Ok(c)
    } else {
        Err(ServiceError::invalid(
            "color",
            "invalid",
            "Use a color like #1f6feb.",
        ))
    }
}

fn due(s: &str) -> Result<Option<time::Date>> {
    let t = s.trim();
    if t.is_empty() {
        return Ok(None);
    }
    let fmt = time::macros::format_description!("[year]-[month]-[day]");
    time::Date::parse(t, &fmt)
        .map(Some)
        .map_err(|_| ServiceError::invalid("due_on", "invalid", "Use a date like 2026-12-31."))
}

fn user_of(subject: &Subject) -> Option<UserId> {
    match subject {
        Subject::User { id, .. } => Some(*id),
        _ => None,
    }
}

fn conflict(field: &'static str, what: &'static str) -> impl Fn(slath_db::DbError) -> ServiceError {
    move |e| match e {
        slath_db::DbError::Conflict(_) => {
            ServiceError::invalid(field, "taken", format!("A {what} with this name exists."))
        }
        other => other.into(),
    }
}

impl Services {
    pub async fn labels(
        &self,
        subject: &Subject,
        ns: &str,
        repo: &str,
    ) -> Result<(RepoAccess, Vec<Label>)> {
        let a = self
            .issues_access(subject, ns, repo, Action::RepoRead)
            .await?;
        let l = slath_db::labels::labels(&self.pool, a.repo.id).await?;
        Ok((a, l))
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn save_label(
        &self,
        subject: &Subject,
        ns: &str,
        repo: &str,
        id: Option<uuid::Uuid>,
        name: &str,
        col: &str,
        description: &str,
    ) -> Result<()> {
        let a = self
            .issues_access(subject, ns, repo, Action::LabelsManage)
            .await?;
        let mut errors: Vec<FieldError> = Vec::new();
        let name = clean("name", name, 50, "name")
            .map_err(|e| errors.extend(fields(e)))
            .ok();
        let col = color(col).map_err(|e| errors.extend(fields(e))).ok();
        let desc = optional("description", description, 200)
            .map_err(|e| errors.extend(fields(e)))
            .ok();
        let (Some(name), Some(col), Some(desc)) = (name, col, desc) else {
            return Err(ServiceError::Invalid(errors));
        };
        match id {
            None => {
                slath_db::labels::create_label(&self.pool, a.repo.id, &name, &col, desc.as_deref())
                    .await
                    .map_err(conflict("name", "label"))?;
            }
            Some(id) => {
                if !slath_db::labels::update_label(
                    &self.pool,
                    a.repo.id,
                    id,
                    &name,
                    &col,
                    desc.as_deref(),
                )
                .await
                .map_err(conflict("name", "label"))?
                {
                    return Err(ServiceError::NotFound);
                }
            }
        }
        Ok(())
    }

    pub async fn delete_label(
        &self,
        subject: &Subject,
        ns: &str,
        repo: &str,
        id: uuid::Uuid,
    ) -> Result<()> {
        let a = self
            .issues_access(subject, ns, repo, Action::LabelsManage)
            .await?;
        if !slath_db::labels::delete_label(&self.pool, a.repo.id, id).await? {
            return Err(ServiceError::NotFound);
        }
        Ok(())
    }

    pub async fn milestones(
        &self,
        subject: &Subject,
        ns: &str,
        repo: &str,
    ) -> Result<(RepoAccess, Vec<Milestone>)> {
        let a = self
            .issues_access(subject, ns, repo, Action::RepoRead)
            .await?;
        let m = slath_db::labels::milestones(&self.pool, a.repo.id).await?;
        Ok((a, m))
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn save_milestone(
        &self,
        subject: &Subject,
        ns: &str,
        repo: &str,
        id: Option<uuid::Uuid>,
        title: &str,
        description: &str,
        due_on: &str,
        open: bool,
    ) -> Result<()> {
        let a = self
            .issues_access(subject, ns, repo, Action::MilestonesManage)
            .await?;
        let mut errors: Vec<FieldError> = Vec::new();
        let title = clean("title", title, 100, "title")
            .map_err(|e| errors.extend(fields(e)))
            .ok();
        let desc = optional("description", description, 2000)
            .map_err(|e| errors.extend(fields(e)))
            .ok();
        let due = due(due_on).map_err(|e| errors.extend(fields(e))).ok();
        let (Some(title), Some(desc), Some(due)) = (title, desc, due) else {
            return Err(ServiceError::Invalid(errors));
        };
        match id {
            None => {
                slath_db::labels::create_milestone(
                    &self.pool,
                    a.repo.id,
                    &title,
                    desc.as_deref(),
                    due,
                )
                .await
                .map_err(conflict("title", "milestone"))?;
            }
            Some(id) => {
                if !slath_db::labels::update_milestone(
                    &self.pool,
                    a.repo.id,
                    id,
                    &title,
                    desc.as_deref(),
                    due,
                    open,
                )
                .await
                .map_err(conflict("title", "milestone"))?
                {
                    return Err(ServiceError::NotFound);
                }
            }
        }
        Ok(())
    }

    pub async fn delete_milestone(
        &self,
        subject: &Subject,
        ns: &str,
        repo: &str,
        id: uuid::Uuid,
    ) -> Result<()> {
        let a = self
            .issues_access(subject, ns, repo, Action::MilestonesManage)
            .await?;
        if !slath_db::labels::delete_milestone(&self.pool, a.repo.id, id).await? {
            return Err(ServiceError::NotFound);
        }
        Ok(())
    }

    /// Labels and assignees of issues (for lists and the issue page).
    pub async fn issue_meta(&self, issues: &[uuid::Uuid]) -> Result<IssueMeta> {
        let mut m = IssueMeta::default();
        for (issue, label) in slath_db::labels::issue_labels(&self.pool, issues).await? {
            m.labels.entry(issue).or_default().push(label);
        }
        for (issue, _, name) in slath_db::labels::assignees(&self.pool, issues).await? {
            m.assignees.entry(issue).or_default().push(name);
        }
        Ok(m)
    }

    /// Resolves filters typed by name. An unknown name matches nothing.
    pub async fn resolve_filter(&self, a: &RepoAccess, f: &FilterInput) -> Result<Filter> {
        let nothing = uuid::Uuid::nil();
        let user = |name: &Option<String>| {
            let name = name
                .as_deref()
                .map(str::trim)
                .filter(|n| !n.is_empty())
                .map(str::to_owned);
            async move {
                match name {
                    None => Ok::<_, ServiceError>(None),
                    Some(n) => Ok(Some(
                        match slath_db::orgs::resolve_namespace(&self.pool, &n).await? {
                            Some((_, _, slath_db::orgs::NamespaceKind::User(u))) => u,
                            _ => UserId::from_uuid(nothing),
                        },
                    )),
                }
            }
        };
        let wanted: Vec<String> = f
            .labels
            .iter()
            .map(|l| l.trim().to_owned())
            .filter(|l| !l.is_empty())
            .collect();
        let mut labels: Vec<uuid::Uuid> =
            slath_db::labels::labels_by_name(&self.pool, a.repo.id, &wanted)
                .await?
                .into_iter()
                .map(|l| l.id)
                .collect();
        if labels.len()
            < wanted
                .iter()
                .map(|w| w.to_lowercase())
                .collect::<BTreeSet<_>>()
                .len()
        {
            labels.push(nothing); // an unknown label: nothing can have all of them
        }
        let milestone = match f
            .milestone
            .as_deref()
            .map(str::trim)
            .filter(|m| !m.is_empty())
        {
            None => None,
            Some(t) => Some(
                slath_db::labels::milestones(&self.pool, a.repo.id)
                    .await?
                    .into_iter()
                    .find(|m| m.title.eq_ignore_ascii_case(t))
                    .map_or(nothing, |m| m.id),
            ),
        };
        Ok(Filter {
            state: match f.state.as_deref() {
                Some("all") => None,
                Some("closed") => Some("closed".to_owned()),
                _ => Some("open".to_owned()),
            },
            author: user(&f.author).await?,
            assignee: user(&f.assignee).await?,
            labels,
            milestone,
            sort_updated: f.sort_updated,
        })
    }

    async fn triage_issue(
        &self,
        subject: &Subject,
        ns: &str,
        repo: &str,
        number: i32,
    ) -> Result<(RepoAccess, slath_db::issues::Issue)> {
        let (a, issue) = self.issue(subject, ns, repo, number).await?;
        if !a.can(subject, Action::IssueTriage) {
            return Err(ServiceError::Forbidden);
        }
        Ok((a, issue))
    }

    async fn event(
        &self,
        a: &RepoAccess,
        issue: uuid::Uuid,
        subject: &Subject,
        kind: &str,
        data: serde_json::Value,
    ) -> Result<()> {
        slath_db::issues::insert_event(
            &self.pool,
            a.repo.id,
            "issue",
            issue,
            user_of(subject),
            kind,
            &data,
        )
        .await?;
        Ok(())
    }

    /// Sets an issue's labels (by name) to exactly `names`.
    pub async fn set_issue_labels(
        &self,
        subject: &Subject,
        ns: &str,
        repo: &str,
        number: i32,
        names: &[String],
    ) -> Result<()> {
        let (a, issue) = self.triage_issue(subject, ns, repo, number).await?;
        let wanted: Vec<String> = names
            .iter()
            .map(|n| n.trim().to_owned())
            .filter(|n| !n.is_empty())
            .collect();
        let found = slath_db::labels::labels_by_name(&self.pool, a.repo.id, &wanted).await?;
        if let Some(missing) = wanted
            .iter()
            .find(|w| !found.iter().any(|f| f.name.eq_ignore_ascii_case(w)))
        {
            return Err(ServiceError::invalid(
                "labels",
                "unknown",
                format!("There's no label “{missing}”."),
            ));
        }
        let current: Vec<Label> = slath_db::labels::issue_labels(&self.pool, &[issue.id])
            .await?
            .into_iter()
            .map(|(_, l)| l)
            .collect();
        let mut changed = false;
        for l in &found {
            if !current.iter().any(|c| c.id == l.id)
                && slath_db::labels::add_issue_label(&self.pool, issue.id, l.id).await?
            {
                self.event(
                    &a,
                    issue.id,
                    subject,
                    "labeled",
                    serde_json::json!({"label": l.name}),
                )
                .await?;
                changed = true;
            }
        }
        for c in &current {
            if !found.iter().any(|l| l.id == c.id)
                && slath_db::labels::remove_issue_label(&self.pool, issue.id, c.id).await?
            {
                self.event(
                    &a,
                    issue.id,
                    subject,
                    "unlabeled",
                    serde_json::json!({"label": c.name}),
                )
                .await?;
                changed = true;
            }
        }
        if changed {
            slath_db::issues::touch(&self.pool, issue.id).await?;
        }
        Ok(())
    }

    /// Sets an issue's assignees (by username). Each must be able to read
    /// the repository; at most ten.
    pub async fn set_issue_assignees(
        &self,
        subject: &Subject,
        ns: &str,
        repo: &str,
        number: i32,
        names: &[String],
    ) -> Result<()> {
        let (a, issue) = self.triage_issue(subject, ns, repo, number).await?;
        let wanted: BTreeSet<String> = names
            .iter()
            .map(|n| n.trim().to_lowercase())
            .filter(|n| !n.is_empty())
            .collect();
        if wanted.len() > MAX_ASSIGNEES {
            return Err(ServiceError::invalid(
                "assignees",
                "too_many",
                format!("Assign at most {MAX_ASSIGNEES} people."),
            ));
        }
        let mut ids = Vec::new();
        for name in &wanted {
            let cannot = || {
                ServiceError::invalid(
                    "assignees",
                    "no_access",
                    format!("{name} can't be assigned: they can't read this repository."),
                )
            };
            let Some((_, shown, slath_db::orgs::NamespaceKind::User(u))) =
                slath_db::orgs::resolve_namespace(&self.pool, name).await?
            else {
                return Err(cannot());
            };
            let Some(user) = slath_db::users::get(&self.pool, u).await? else {
                return Err(cannot());
            };
            let them = Subject::User {
                id: u,
                kind: user.kind,
                state: user.state,
                is_site_admin: user.is_admin,
                credential: slath_core::authz::Credential::Session,
            };
            if self
                .repo_access_by_id(&them, a.repo.id, Action::RepoRead)
                .await
                .is_err()
            {
                return Err(cannot());
            }
            ids.push((u, shown));
        }
        let current = slath_db::labels::assignees(&self.pool, &[issue.id]).await?;
        let mut changed = false;
        let mut added = Vec::new();
        for (u, name) in &ids {
            if slath_db::labels::add_assignee(&self.pool, issue.id, *u).await? {
                added.push(*u);
                self.event(
                    &a,
                    issue.id,
                    subject,
                    "assigned",
                    serde_json::json!({"user": name}),
                )
                .await?;
                changed = true;
            }
        }
        for (_, u, name) in &current {
            if !ids.iter().any(|(id, _)| id == u)
                && slath_db::labels::remove_assignee(&self.pool, issue.id, *u).await?
            {
                self.event(
                    &a,
                    issue.id,
                    subject,
                    "unassigned",
                    serde_json::json!({"user": name}),
                )
                .await?;
                changed = true;
            }
        }
        if changed {
            slath_db::issues::touch(&self.pool, issue.id).await?;
        }
        if !added.is_empty() {
            self.notify_item(
                subject,
                &a,
                &issue,
                crate::notifications::Activity::Assigned(added),
            )
            .await;
        }
        Ok(())
    }

    /// Sets (by title) or clears an issue's milestone.
    pub async fn set_issue_milestone(
        &self,
        subject: &Subject,
        ns: &str,
        repo: &str,
        number: i32,
        title: Option<&str>,
    ) -> Result<()> {
        let (a, issue) = self.triage_issue(subject, ns, repo, number).await?;
        let all = slath_db::labels::milestones(&self.pool, a.repo.id).await?;
        let new = match title.map(str::trim).filter(|t| !t.is_empty()) {
            None => None,
            Some(t) => Some(
                all.iter()
                    .find(|m| m.title.eq_ignore_ascii_case(t))
                    .ok_or_else(|| {
                        ServiceError::invalid(
                            "milestone",
                            "unknown",
                            format!("There's no milestone “{t}”."),
                        )
                    })?,
            ),
        };
        if new.map(|m| m.id) == issue.milestone_id {
            return Ok(());
        }
        slath_db::labels::set_issue_milestone(&self.pool, issue.id, new.map(|m| m.id)).await?;
        match new {
            Some(m) => {
                self.event(
                    &a,
                    issue.id,
                    subject,
                    "milestoned",
                    serde_json::json!({"milestone": m.title}),
                )
                .await?
            }
            None => {
                let old = all
                    .iter()
                    .find(|m| Some(m.id) == issue.milestone_id)
                    .map(|m| m.title.clone())
                    .unwrap_or_default();
                self.event(
                    &a,
                    issue.id,
                    subject,
                    "demilestoned",
                    serde_json::json!({"milestone": old}),
                )
                .await?;
            }
        }
        Ok(())
    }
}

fn fields(e: ServiceError) -> Vec<FieldError> {
    match e {
        ServiceError::Invalid(f) => f,
        _ => Vec::new(),
    }
}
