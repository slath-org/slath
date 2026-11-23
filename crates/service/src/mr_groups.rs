// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Linked merge requests: an ordered group of merge requests, in one or
//! several repositories of this instance, that merge together. Git can't
//! land in several repositories atomically, so a group merge checks every
//! member first and then merges them in order, each with its own
//! requirements; when a later merge fails it stops and the group shows
//! what landed and what remains. A linked merge request merges only
//! through its group.

use slath_core::authz::{Action, Subject};
use slath_core::ids::MergeRequestId;
use slath_db::merge_requests::MergeRequest;

use crate::Services;
use crate::error::{Result, ServiceError};
use crate::merge::MergeInput;
use crate::repos::RepoAccess;

/// Refusal of a linked merge request's own merge or merge when ready.
pub const LINKED: &str = "This merge request is linked with others to land together: merge the group, or unlink it first.";
/// Refusal for someone who may not merge every member.
pub const NOT_ALL: &str =
    "You can merge this group only if you may merge every merge request in it.";

/// One member as the requester sees it.
#[derive(Debug, Clone)]
pub struct Member {
    pub namespace: String,
    pub repo: String,
    pub number: i32,
    pub title: String,
    /// Who wrote the title.
    pub origin: slath_core::origin::Origin,
    /// `open`, `merged`, or `closed`.
    pub state: String,
    pub head_oid: Option<String>,
    /// The readiness card's tone and headline.
    pub tone: String,
    pub headline: String,
    /// Mergeable now with all its requirements.
    pub mergeable: bool,
    /// The requester may merge it (`mr.merge` on its repository).
    pub can_merge: bool,
    /// The merge request the group was asked for.
    pub current: bool,
}

impl Member {
    /// `ns/repo!n`.
    #[must_use]
    pub fn reference(&self) -> String {
        format!("{}/{}!{}", self.namespace, self.repo, self.number)
    }
}

/// Why the last group merge stopped.
#[derive(Debug, Clone)]
pub struct Failure {
    /// The member it stopped at (`ns/repo!n`), when the requester can read it.
    pub member: Option<String>,
    pub reason: String,
    pub at: Option<time::OffsetDateTime>,
}

/// A group as the requester sees it: only members they can read, in order.
#[derive(Debug, Clone)]
pub struct GroupView {
    /// `open`, `partially_landed`, or `merged`, from every member.
    pub state: &'static str,
    pub members: Vec<Member>,
    pub last_failure: Option<Failure>,
    /// The requester may change the group (edit every member).
    pub can_edit: bool,
    /// Every member is readable and the requester may merge each open one.
    pub can_merge: bool,
    /// Every open member is mergeable now.
    pub ready: bool,
    /// A group merge runs now.
    pub merging: bool,
}

impl GroupView {
    /// The open members, in order.
    pub fn open(&self) -> impl Iterator<Item = &Member> {
        self.members.iter().filter(|m| m.state == "open")
    }
}

/// One member of a group merge request.
#[derive(Debug, Clone, Default)]
pub struct MergeMember {
    /// `ns/repo!n`.
    pub merge_request: String,
    pub expected_head_oid: String,
    /// `None`: the repository's default.
    pub strategy: Option<String>,
}

/// What a group merge takes: every open member in the group's order.
#[derive(Debug, Clone, Default)]
pub struct GroupMergeInput {
    pub members: Vec<MergeMember>,
    /// `None`: each merge request's own setting.
    pub delete_source: Option<bool>,
}

/// What a group merge did.
#[derive(Debug, Clone)]
pub struct GroupMergeOutcome {
    pub group: GroupView,
    /// Merged by this group merge, in order (`ns/repo!n`).
    pub merged: Vec<String>,
    /// Still open (`ns/repo!n`).
    pub remaining: Vec<String>,
    /// Why it stopped, when it did.
    pub failure: Option<String>,
}

/// `ns/repo!n`, `ns/repo#n`, or a merge request URL (or its path).
#[must_use]
pub fn parse_reference(s: &str) -> Option<(String, String, i32)> {
    let s = s.trim();
    let path = match s.find("://") {
        Some(i) => {
            let rest = &s[i + 3..];
            &rest[rest.find('/')?..]
        }
        None => s,
    };
    let path = path.trim_start_matches('/');
    let path = path.split(['?', '#']).next().unwrap_or(path);
    let parts: Vec<&str> = path.trim_end_matches('/').split('/').collect();
    if let [ns, repo, "merge-requests", n, ..] = parts.as_slice() {
        return Some(((*ns).to_owned(), (*repo).to_owned(), n.parse().ok()?));
    }
    let (ns, rest) = s.split_once('/')?;
    let (repo, n) = rest.split_once('!').or_else(|| rest.split_once('#'))?;
    if ns.is_empty() || repo.is_empty() || repo.contains('/') {
        return None;
    }
    Some((ns.to_owned(), repo.to_owned(), n.parse().ok()?))
}

impl Services {
    /// Whether the merge request is in a linked group (it then merges only
    /// with its group).
    pub async fn mr_is_linked(&self, m: &MergeRequest) -> Result<bool> {
        self.mr_linked(m.id).await
    }

    /// Whether `m` is in a linked group.
    pub(crate) async fn mr_linked(&self, m: MergeRequestId) -> Result<bool> {
        let mut c = self.pool.acquire().await.map_err(ServiceError::internal)?;
        Ok(slath_db::mr_groups::of(&mut c, m).await?.is_some())
    }

    /// A merge request and its repository by ID, if the subject can read
    /// both (merge requests on).
    async fn member(
        &self,
        subject: &Subject,
        id: MergeRequestId,
    ) -> Result<Option<(RepoAccess, MergeRequest)>> {
        let Some(m) = slath_db::merge_requests::get_by_id(&self.pool, id).await? else {
            return Ok(None);
        };
        let a = match self
            .repo_access_by_id(subject, m.repo_id, Action::RepoRead)
            .await
        {
            Ok(a) => a,
            Err(ServiceError::NotFound | ServiceError::Forbidden) => return Ok(None),
            Err(e) => return Err(e),
        };
        if !self.mrs_enabled(&a).await {
            return Ok(None);
        }
        Ok(Some((a, m)))
    }

    /// The group of the merge request `n` as the subject sees it; `None`
    /// when it isn't linked.
    pub async fn mr_group(
        &self,
        subject: &Subject,
        (ns, repo, number): (&str, &str, i32),
    ) -> Result<Option<GroupView>> {
        let (_, m) = self.mr(subject, ns, repo, number).await?;
        self.group_view(subject, m.id).await
    }

    async fn group_view(&self, subject: &Subject, of: MergeRequestId) -> Result<Option<GroupView>> {
        let mut c = self.pool.acquire().await.map_err(ServiceError::internal)?;
        let Some(g) = slath_db::mr_groups::of(&mut c, of).await? else {
            return Ok(None);
        };
        let merging = slath_db::mr_groups::merging(&mut *c, g.id).await?;
        drop(c);
        let mut members = Vec::new();
        let mut all_readable = true;
        let mut can_edit = true;
        let (mut merged, mut total) = (0, 0);
        let mut failure_member = None;
        let failed_at = g
            .last_failure
            .as_ref()
            .and_then(|f| f["mr_id"].as_str())
            .and_then(|s| s.parse::<uuid::Uuid>().ok());
        for id in &g.members {
            let Some((a, m)) = self.member(subject, *id).await? else {
                all_readable = false;
                can_edit = false;
                // The state counts every member, readable or not.
                if let Some(m) = slath_db::merge_requests::get_by_id(&self.pool, *id).await? {
                    total += 1;
                    merged += i32::from(m.state == "merged");
                }
                continue;
            };
            total += 1;
            merged += i32::from(m.state == "merged");
            can_edit &= self.mr_perms(subject, &a, &m).edit;
            let (tone, headline, mergeable) = if m.state == "open" {
                let mb = self.mr_mergeability(&a, &m).await?;
                let (t, h) = crate::mr_reviews::readiness_summary(&m, &mb);
                (t.to_owned(), h, mb.mergeable)
            } else {
                let (t, h) = crate::mr_reviews::readiness_summary(
                    &m,
                    &crate::mr_reviews::Mergeability::default(),
                );
                (t.to_owned(), h, false)
            };
            let member = Member {
                namespace: a.repo.namespace.clone(),
                repo: a.repo.name.clone(),
                number: m.number,
                title: m.title.clone(),
                origin: m.origin,
                state: m.state.clone(),
                head_oid: m.head_oid.clone(),
                tone,
                headline,
                mergeable,
                can_merge: a.can(subject, Action::MrMerge),
                current: m.id == of,
            };
            if failed_at == Some(*m.id.as_uuid()) {
                failure_member = Some(member.reference());
            }
            members.push(member);
        }
        let state = if total > 0 && merged == total {
            "merged"
        } else if merged > 0 {
            "partially_landed"
        } else {
            "open"
        };
        let last_failure = g.last_failure.as_ref().map(|f| Failure {
            // The reason may name the member; only readers of it see it.
            reason: if failure_member.is_some() || failed_at.is_none() {
                f["reason"].as_str().unwrap_or_default().to_owned()
            } else {
                "A merge request you can't see couldn't be merged.".to_owned()
            },
            member: failure_member,
            at: f["at"].as_str().and_then(|t| {
                time::OffsetDateTime::parse(t, &time::format_description::well_known::Rfc3339).ok()
            }),
        });
        let open: Vec<&Member> = members.iter().filter(|m| m.state == "open").collect();
        Ok(Some(GroupView {
            state,
            can_edit,
            can_merge: all_readable && open.iter().all(|m| m.can_merge),
            ready: !open.is_empty() && open.iter().all(|m| m.mergeable),
            merging,
            last_failure,
            members,
        }))
    }

    /// Sets the group of the merge request `n` to `members` (references,
    /// in order, including `n`): creates, extends, shrinks, or reorders it.
    pub async fn set_mr_group(
        &self,
        subject: &Subject,
        (ns, repo, number): (&str, &str, i32),
        members: &[String],
    ) -> Result<GroupView> {
        let (a, m) = self.mr(subject, ns, repo, number).await?;
        if !self.mr_perms(subject, &a, &m).edit {
            return Err(ServiceError::Forbidden);
        }
        let most = self.config.limits.mr_group_members;
        if members.len() < 2 || members.len() > most {
            return Err(ServiceError::invalid(
                "members",
                "count",
                format!(
                    "A linked group holds 2 to {most} merge requests (limits.mr_group_members)."
                ),
            ));
        }
        // Resolve every reference; an unreadable one is not found.
        let mut resolved: Vec<(RepoAccess, MergeRequest)> = Vec::new();
        for r in members {
            let (rns, rrepo, rn) = parse_reference(r).ok_or_else(|| {
                ServiceError::invalid(
                    "members",
                    "reference",
                    format!("{r:?} isn't a merge request reference like acme/app!7."),
                )
            })?;
            let (ra, rm) = self.mr(subject, &rns, &rrepo, rn).await?;
            if resolved.iter().any(|(_, x)| x.id == rm.id) {
                return Err(ServiceError::invalid(
                    "members",
                    "duplicate",
                    format!("{r} is named twice."),
                ));
            }
            resolved.push((ra, rm));
        }
        if !resolved.iter().any(|(_, x)| x.id == m.id) {
            return Err(ServiceError::invalid(
                "members",
                "missing",
                "The list must include this merge request.",
            ));
        }
        let mut tx = self.pool.begin().await.map_err(ServiceError::internal)?;
        let existing = slath_db::mr_groups::of(&mut tx, m.id).await?;
        if let Some(g) = &existing {
            slath_db::mr_groups::lock(&mut *tx, g.id).await?;
            if slath_db::mr_groups::merging(&mut *tx, g.id).await? {
                return Err(ServiceError::Conflict(
                    "The group is merging; wait for it to finish.".into(),
                ));
            }
            // Changing a group needs the edit right on all its members.
            for id in &g.members {
                let Some((xa, xm)) = self.member(subject, *id).await? else {
                    return Err(ServiceError::Forbidden);
                };
                if !self.mr_perms(subject, &xa, &xm).edit {
                    return Err(ServiceError::Forbidden);
                }
            }
        }
        let current: Vec<MergeRequestId> = existing
            .as_ref()
            .map(|g| g.members.clone())
            .unwrap_or_default();
        let mut seen_open = false;
        for (ra, rm) in &resolved {
            if !self.mr_perms(subject, ra, rm).edit {
                return Err(ServiceError::Forbidden);
            }
            let name = format!("{}/{}!{}", ra.repo.namespace, ra.repo.name, rm.number);
            if current.contains(&rm.id) && rm.state == "merged" {
                if seen_open {
                    return Err(ServiceError::invalid(
                        "members",
                        "order",
                        format!("{name} is merged already; merged merge requests come first."),
                    ));
                }
                continue;
            }
            seen_open = true;
            if rm.state != "open" {
                return Err(ServiceError::Conflict(format!("{name} isn't open.")));
            }
            if !current.contains(&rm.id) {
                let mut c = self.pool.acquire().await.map_err(ServiceError::internal)?;
                if slath_db::mr_groups::of(&mut c, rm.id).await?.is_some() {
                    return Err(ServiceError::Conflict(format!(
                        "{name} is linked in another group already; unlink it there first."
                    )));
                }
                if slath_db::merge_requests::auto_merge(&mut *c, rm.id)
                    .await?
                    .is_some()
                {
                    return Err(ServiceError::Conflict(format!(
                        "{name} merges when ready. Cancel merge when ready first."
                    )));
                }
            }
        }
        // Merged members can't be dropped: they landed as part of it.
        for id in &current {
            if !resolved.iter().any(|(_, x)| x.id == *id)
                && let Some(x) = slath_db::merge_requests::get_by_id(&mut *tx, *id).await?
                && x.state == "merged"
            {
                return Err(ServiceError::invalid(
                    "members",
                    "merged",
                    "Merged merge requests stay in the group; name them too.",
                ));
            }
        }
        let group = match &existing {
            Some(g) => g.id,
            None => slath_db::mr_groups::create(&mut *tx, subject_user(subject)).await?,
        };
        let ids: Vec<MergeRequestId> = resolved.iter().map(|(_, x)| x.id).collect();
        slath_db::mr_groups::set_members(&mut tx, group, &ids).await?;
        tx.commit().await.map_err(ServiceError::internal)?;
        let names: Vec<String> = resolved
            .iter()
            .map(|(ra, rm)| format!("{}/{}!{}", ra.repo.namespace, ra.repo.name, rm.number))
            .collect();
        self.audit(
            subject,
            "mr.group.update",
            crate::audit::Target::new("merge_request", Some(*m.id.as_uuid()))
                .in_repo(*a.repo.id.as_uuid()),
            serde_json::json!({"group": group, "members": names}),
        )
        .await;
        self.group_view(subject, m.id)
            .await?
            .ok_or(ServiceError::NotFound)
    }

    /// Unlinks the merge request `n` from its group; a group left with
    /// fewer than two members dissolves.
    pub async fn unlink_mr_group(
        &self,
        subject: &Subject,
        (ns, repo, number): (&str, &str, i32),
    ) -> Result<()> {
        let (a, m) = self.mr(subject, ns, repo, number).await?;
        if !self.mr_perms(subject, &a, &m).edit {
            return Err(ServiceError::Forbidden);
        }
        let mut tx = self.pool.begin().await.map_err(ServiceError::internal)?;
        let Some(g) = slath_db::mr_groups::of(&mut tx, m.id).await? else {
            return Err(ServiceError::NotFound);
        };
        slath_db::mr_groups::lock(&mut *tx, g.id).await?;
        if slath_db::mr_groups::merging(&mut *tx, g.id).await? {
            return Err(ServiceError::Conflict(
                "The group is merging; wait for it to finish.".into(),
            ));
        }
        if m.state == "merged" {
            return Err(ServiceError::Conflict(
                "Merged merge requests stay in the group.".into(),
            ));
        }
        let rest: Vec<MergeRequestId> = g.members.iter().copied().filter(|x| *x != m.id).collect();
        if rest.len() < 2 {
            slath_db::mr_groups::delete(&mut *tx, g.id).await?;
        } else {
            slath_db::mr_groups::set_members(&mut tx, g.id, &rest).await?;
        }
        tx.commit().await.map_err(ServiceError::internal)?;
        self.audit(
            subject,
            "mr.group.update",
            crate::audit::Target::new("merge_request", Some(*m.id.as_uuid()))
                .in_repo(*a.repo.id.as_uuid()),
            serde_json::json!({"group": g.id, "unlinked": format!("{ns}/{repo}!{number}")}),
        )
        .await;
        Ok(())
    }
}

impl Services {
    /// Merges the group of the merge request `n`: checks every open member
    /// first (nothing merges unless all are mergeable), then merges them
    /// in order, each as a single merge with its own requirements. A
    /// failure after the first merge stops and leaves the group partially
    /// landed; a failure before it is that merge's error.
    pub async fn merge_mr_group(
        &self,
        subject: &Subject,
        (ns, repo, number): (&str, &str, i32),
        input: &GroupMergeInput,
    ) -> Result<GroupMergeOutcome> {
        let (_, m) = self.mr(subject, ns, repo, number).await?;
        let user = subject_user(subject).ok_or(ServiceError::Unauthenticated)?;
        let g = {
            let mut c = self.pool.acquire().await.map_err(ServiceError::internal)?;
            slath_db::mr_groups::of(&mut c, m.id)
                .await?
                .ok_or(ServiceError::NotFound)?
        };
        // Every member must be readable and mergeable by the subject.
        let mut open: Vec<(RepoAccess, MergeRequest)> = Vec::new();
        for id in &g.members {
            let Some((a, x)) = self.member(subject, *id).await? else {
                return Err(ServiceError::Conflict(NOT_ALL.into()));
            };
            if x.state == "open" {
                if !a.can(subject, Action::MrMerge) {
                    return Err(ServiceError::Conflict(NOT_ALL.into()));
                }
                open.push((a, x));
            }
        }
        let name = member_name;
        // The request names every open member, in order.
        let named: Vec<Option<(String, String, i32)>> = input
            .members
            .iter()
            .map(|r| parse_reference(&r.merge_request))
            .collect();
        let same = named.len() == open.len()
            && named.iter().zip(&open).all(|(n, (a, x))| {
                n.as_ref().is_some_and(|(rns, rrepo, rn)| {
                    *rns == a.repo.namespace && *rrepo == a.repo.name && *rn == x.number
                })
            });
        if !same || open.is_empty() {
            return Err(ServiceError::Conflict(
                "The group changed since you looked; reload it.".into(),
            ));
        }
        if !slath_db::mr_groups::claim(&self.pool, g.id).await? {
            return Err(ServiceError::Conflict(
                "The group is merging already; wait for it to finish.".into(),
            ));
        }
        let outcome = self.merge_claimed(subject, &open, input).await;
        let failure = match &outcome {
            Ok((_, Some((id, reason)))) | Err((Some((id, reason)), _)) => Some(serde_json::json!({
                "mr_id": id.as_uuid(),
                "reason": reason,
                "at": time::OffsetDateTime::now_utc()
                    .format(&time::format_description::well_known::Rfc3339)
                    .unwrap_or_default(),
                "actor": user.as_uuid(),
            })),
            Ok((_, None)) => None,
            // Refused before merging: the earlier failure still holds.
            Err((None, _)) => g.last_failure.clone(),
        };
        slath_db::mr_groups::finish(&self.pool, g.id, failure.as_ref()).await?;
        let (merged, failure) = match outcome {
            Ok((merged, f)) => (merged, f.map(|(_, r)| r)),
            Err((_, e)) => return Err(e),
        };
        let remaining: Vec<String> = open
            .iter()
            .filter(|p| !merged.contains(&name(p)))
            .map(name)
            .collect();
        self.audit(
            subject,
            "mr.group.merge",
            crate::audit::Target::new("merge_request", Some(*m.id.as_uuid())),
            serde_json::json!({
                "group": g.id,
                "merged": merged,
                "remaining": remaining,
                "failure": failure,
            }),
        )
        .await;
        Ok(GroupMergeOutcome {
            group: self
                .group_view(subject, m.id)
                .await?
                .ok_or(ServiceError::NotFound)?,
            merged,
            remaining,
            failure,
        })
    }

    /// Checks, then merges the open members of a claimed group. `Ok` with
    /// what merged and, when it stopped after a merge, at which member and
    /// why; `Err` when nothing merged (with the member that failed, if it
    /// came to merging).
    #[allow(clippy::type_complexity)]
    async fn merge_claimed(
        &self,
        subject: &Subject,
        open: &[(RepoAccess, MergeRequest)],
        input: &GroupMergeInput,
    ) -> std::result::Result<
        (Vec<String>, Option<(MergeRequestId, String)>),
        (Option<(MergeRequestId, String)>, ServiceError),
    > {
        let name = member_name;
        // 1. Check every member; collect every reason.
        let mut refusals: Vec<String> = Vec::new();
        let mut inputs: Vec<MergeInput> = Vec::new();
        for (p, req) in open.iter().zip(&input.members) {
            let (a, x) = p;
            match self.merge_precheck(subject, a, x, req).await {
                Ok(i) => inputs.push(MergeInput {
                    delete_source: input.delete_source,
                    ..i
                }),
                Err(why) => refusals.push(format!("{}: {why}", name(p))),
            }
        }
        if !refusals.is_empty() {
            return Err((
                None,
                ServiceError::Conflict(format!(
                    "The group can't be merged yet. {}",
                    refusals.join(" ")
                )),
            ));
        }
        // 2. Merge in order; stop at the first failure.
        let mut merged = Vec::new();
        for (p, i) in open.iter().zip(inputs) {
            let (a, x) = p;
            match self.merge_one(subject, (a.clone(), x.clone()), &i).await {
                Ok(_) => merged.push(name(p)),
                Err(e) => {
                    let why = failure_text(&e);
                    if merged.is_empty() {
                        return Err((Some((x.id, why)), e));
                    }
                    return Ok((merged, Some((x.id, why))));
                }
            }
        }
        Ok((merged, None))
    }

    /// Whether one member could merge now, as a single merge checks it,
    /// without writing anything: the merge's input, or why not.
    async fn merge_precheck(
        &self,
        subject: &Subject,
        a: &RepoAccess,
        x: &MergeRequest,
        req: &MergeMember,
    ) -> std::result::Result<MergeInput, String> {
        let text = |e: ServiceError| failure_text(&e);
        self.check_bot_merge(subject, a, &x.target_branch)
            .await
            .map_err(text)?;
        if x.head_oid.as_deref() != Some(req.expected_head_oid.as_str()) {
            return Err(format!(
                "It changed since you looked: it's at revision {} now.",
                x.revisions
            ));
        }
        let (allowed, default) = slath_db::repos::merge_settings(&self.pool, a.repo.id)
            .await
            .map_err(|e| text(e.into()))?;
        let strategy = req
            .strategy
            .clone()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or(default);
        if !allowed.contains(&strategy) {
            return Err(format!(
                "Its repository doesn't allow {strategy}; use {}.",
                allowed.join(", ")
            ));
        }
        if strategy == "merge-commit" || strategy == "squash" {
            self.scan_message(a, x, &strategy, None)
                .await
                .map_err(text)?;
        }
        let mb = self.mr_mergeability(a, x).await.map_err(text)?;
        if !mb.mergeable {
            let why: Vec<String> = mb
                .requirements
                .iter()
                .filter(|r| r.state != "met")
                .map(|r| r.reason.clone())
                .collect();
            return Err(why.join(" "));
        }
        Ok(MergeInput {
            strategy: Some(strategy),
            expected_head_oid: req.expected_head_oid.clone(),
            ..MergeInput::default()
        })
    }
}

fn member_name((a, x): &(RepoAccess, MergeRequest)) -> String {
    format!("{}/{}!{}", a.repo.namespace, a.repo.name, x.number)
}

fn failure_text(e: &ServiceError) -> String {
    match e {
        ServiceError::Conflict(w) => w.clone(),
        ServiceError::Invalid(errs) => errs
            .iter()
            .map(|f| f.message.clone())
            .collect::<Vec<_>>()
            .join(" "),
        ServiceError::Forbidden | ServiceError::NotFound => NOT_ALL.to_owned(),
        _ => "The merge failed; try again.".to_owned(),
    }
}

fn subject_user(subject: &Subject) -> Option<slath_core::ids::UserId> {
    match subject {
        Subject::User { id, .. } => Some(*id),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::parse_reference;

    #[test]
    fn references() {
        let r = |s| parse_reference(s);
        let want = Some(("acme".to_owned(), "app".to_owned(), 7));
        assert_eq!(r("acme/app!7"), want);
        assert_eq!(r(" acme/app#7 "), want);
        assert_eq!(r("https://slath.example/acme/app/merge-requests/7"), want);
        assert_eq!(
            r("https://slath.example/acme/app/merge-requests/7/changes?x=1"),
            want
        );
        assert_eq!(r("/acme/app/merge-requests/7"), want);
        assert_eq!(r("acme/app!x"), None);
        assert_eq!(r("acme!7"), None);
        assert_eq!(r("a/b/c!7"), None);
        assert_eq!(r("/app!7"), None);
    }
}
