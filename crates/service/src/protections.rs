// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Branch protection: managing the rules (repository admins) and what they
//! mean for a push (pre-receive step 4).
//!
//! A rule's pattern is a glob on the short branch name: `*` matches any
//! characters except `/`, `?` one such character. When several rules match
//! a branch, the strictest value of each setting applies.

use slath_core::authz::{Action, Role, Subject};
use slath_core::ids::UserId;
use slath_db::protections::{NewProtection, Protection};

use crate::Services;
use crate::error::{Result, ServiceError};

/// A status context that must succeed, and whose reports count.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequiredCheck {
    pub context: String,
    /// `None`: anyone allowed to report statuses (but never the merge
    /// request's author or their bots).
    pub reporters: Option<Vec<UserId>>,
}

/// The settings that apply to one branch (all matching rules combined).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Effective {
    pub protected: bool,
    pub block_force_push: bool,
    pub block_deletion: bool,
    pub require_mr: bool,
    /// Minimum role for direct pushes; `None` with `protected` means nobody.
    pub push_role: Option<Role>,
    pub nobody: bool,
    /// Merge request requirements (the highest count, any rule's flags).
    pub required_approvals: i32,
    pub dismiss_stale_approvals: bool,
    pub require_resolved_threads: bool,
    pub require_up_to_date: bool,
    /// Only if every matching rule lets bots approve.
    pub bots_count_as_approvers: bool,
    /// Every rule's required contexts; with reporter lists from several
    /// rules, only reporters on all of them.
    pub required_checks: Vec<RequiredCheck>,
    /// Only if every matching rule accepts statuses on the head.
    pub statuses_on_head_ok: bool,
    /// Any rule's: every new revision makes earlier approvals stale.
    pub dismiss_approvals_on_new_revision: bool,
    /// Bots that may merge into and push to the branch: those every
    /// matching rule lists.
    pub bot_mergers: Vec<UserId>,
}

impl Effective {
    /// Whether `subject` is a bot that the protection keeps from merging
    /// into or pushing to the branch.
    #[must_use]
    pub fn denies_bot(&self, subject: &Subject) -> bool {
        match subject {
            Subject::User {
                id,
                kind: slath_core::authz::UserKind::Bot,
                ..
            } => self.protected && !self.bot_mergers.contains(id),
            _ => false,
        }
    }
}

/// Why a bot can't merge or push.
pub const BOT_DENIED: &str =
    "bots can merge into or push to this protected branch only if its protection lists them";

/// The reporter lists of a rule: context -> user IDs.
fn reporters_of(p: &Protection) -> std::collections::HashMap<String, Vec<UserId>> {
    let mut out = std::collections::HashMap::new();
    if let Some(map) = p.status_reporters.as_object() {
        for (ctx, ids) in map {
            let ids: Vec<UserId> = ids
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|v| v.as_str()?.parse::<uuid::Uuid>().ok())
                .map(UserId::from_uuid)
                .collect();
            out.insert(ctx.clone(), ids);
        }
    }
    out
}

/// Glob match of `pattern` on a short branch name.
#[must_use]
pub fn glob_match(pattern: &[u8], name: &[u8]) -> bool {
    match pattern.split_first() {
        None => name.is_empty(),
        Some((b'*', rest)) => {
            // Try every split that doesn't cross a '/'.
            let mut i = 0;
            loop {
                if glob_match(rest, &name[i..]) {
                    return true;
                }
                if i == name.len() || name[i] == b'/' {
                    return false;
                }
                i += 1;
            }
        }
        Some((b'?', rest)) => {
            matches!(name.split_first(), Some((c, tail)) if *c != b'/' && glob_match(rest, tail))
        }
        Some((p, rest)) => {
            matches!(name.split_first(), Some((c, tail)) if c == p && glob_match(rest, tail))
        }
    }
}

fn push_role(s: &str) -> Option<Role> {
    match s {
        "write" => Some(Role::Write),
        "maintain" => Some(Role::Maintain),
        "admin" => Some(Role::Admin),
        _ => None,
    }
}

/// Combines the rules that match `branch`.
#[must_use]
pub fn effective(rules: &[Protection], branch: &[u8]) -> Effective {
    let mut e = Effective {
        bots_count_as_approvers: true,
        statuses_on_head_ok: true,
        ..Effective::default()
    };
    let mut bots: Option<Vec<UserId>> = None;
    for r in rules
        .iter()
        .filter(|r| glob_match(r.pattern.as_bytes(), branch))
    {
        let listed: Vec<UserId> = r
            .bot_merge_allowlist
            .iter()
            .copied()
            .map(UserId::from_uuid)
            .collect();
        bots = Some(match bots {
            None => listed,
            Some(b) => b.into_iter().filter(|u| listed.contains(u)).collect(),
        });
        e.protected = true;
        e.required_approvals = e.required_approvals.max(r.required_approvals);
        e.dismiss_stale_approvals |= r.dismiss_stale_approvals;
        e.require_resolved_threads |= r.require_resolved_threads;
        e.require_up_to_date |= r.require_up_to_date;
        e.bots_count_as_approvers &= r.bots_count_as_approvers;
        e.statuses_on_head_ok &= r.statuses_on_head_ok;
        e.dismiss_approvals_on_new_revision |= r.dismiss_approvals_on_new_revision;
        let lists = reporters_of(r);
        for ctx in &r.required_status_contexts {
            let list = lists.get(ctx).filter(|l| !l.is_empty()).cloned();
            match e.required_checks.iter_mut().find(|c| c.context == *ctx) {
                Some(c) => {
                    c.reporters = match (c.reporters.take(), list) {
                        (Some(a), Some(b)) => {
                            Some(a.into_iter().filter(|u| b.contains(u)).collect())
                        }
                        (a, b) => a.or(b),
                    };
                }
                None => e.required_checks.push(RequiredCheck {
                    context: ctx.clone(),
                    reporters: list,
                }),
            }
        }
        e.block_force_push |= r.block_force_push;
        e.block_deletion |= r.block_deletion;
        e.require_mr |= r.require_mr;
        match push_role(&r.push_role) {
            Some(role) => e.push_role = Some(e.push_role.map_or(role, |p| p.max(role))),
            None => e.nobody = true,
        }
    }
    e.bot_mergers = bots.unwrap_or_default();
    if !e.protected {
        e.bots_count_as_approvers = false;
    }
    e.required_checks.sort_by(|a, b| a.context.cmp(&b.context));
    e
}

fn check_pattern(p: &str) -> Result<()> {
    let ok = !p.is_empty()
        && p.len() <= 255
        && !p.starts_with('/')
        && !p.ends_with('/')
        && !p.contains("//")
        && !p.contains("..")
        && p.bytes()
            .all(|b| b.is_ascii_graphic() && !matches!(b, b'~' | b'^' | b':' | b'\\' | b'['));
    if ok {
        Ok(())
    } else {
        Err(ServiceError::invalid(
            "pattern",
            "invalid",
            "Use a branch name, optionally with * (e.g. main or release/*).",
        ))
    }
}

/// A rule as the settings form submits it.
pub struct RuleInput {
    pub pattern: String,
    pub block_force_push: bool,
    pub block_deletion: bool,
    pub require_mr: bool,
    pub push_role: String,
    pub required_approvals: i32,
    pub dismiss_stale_approvals: bool,
    pub require_resolved_threads: bool,
    pub require_up_to_date: bool,
    pub bots_count_as_approvers: bool,
    /// Required contexts, each with the names of the users or bots whose
    /// statuses count (empty: anyone allowed).
    pub required_checks: Vec<(String, Vec<String>)>,
    pub statuses_on_head_ok: bool,
    pub dismiss_approvals_on_new_revision: bool,
    /// Names of the bots that may merge into and push to matching branches.
    pub bot_merge_allowlist: Vec<String>,
}

/// Whether `s` is a valid status context.
#[must_use]
pub fn valid_context(s: &str) -> bool {
    (1..=100).contains(&s.len())
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b" ._/:-".contains(&b))
}

impl Services {
    /// The repository's rules (repository admins).
    pub async fn protections(
        &self,
        subject: &Subject,
        ns: &str,
        repo: &str,
    ) -> Result<Vec<Protection>> {
        let a = self
            .repo_access(subject, ns, repo, Action::ProtectionManage)
            .await?;
        Ok(slath_db::protections::list(&self.pool, a.repo.id).await?)
    }

    /// Creates or replaces the rule for a pattern. Audited.
    pub async fn set_protection(
        &self,
        subject: &Subject,
        ns: &str,
        repo: &str,
        r: RuleInput,
    ) -> Result<()> {
        let a = self
            .repo_access(subject, ns, repo, Action::ProtectionManage)
            .await?;
        let pattern = r.pattern.trim();
        check_pattern(pattern)?;
        if !matches!(
            r.push_role.as_str(),
            "write" | "maintain" | "admin" | "nobody"
        ) {
            return Err(ServiceError::invalid(
                "push_role",
                "invalid",
                "Choose who may push.",
            ));
        }
        if !(0..=20).contains(&r.required_approvals) {
            return Err(ServiceError::invalid(
                "required_approvals",
                "invalid",
                "Use a number from 0 to 20.",
            ));
        }
        let mut contexts = Vec::new();
        let mut reporters = serde_json::Map::new();
        for (ctx, names) in &r.required_checks {
            let ctx = ctx.trim();
            if !valid_context(ctx) {
                return Err(ServiceError::invalid(
                    "required_checks",
                    "invalid",
                    format!(
                        "“{ctx}” isn't a status context: use up to 100 letters, digits, spaces, and . _ / : -"
                    ),
                ));
            }
            let mut ids = Vec::new();
            for n in names
                .iter()
                .map(|n| n.trim().trim_start_matches('@'))
                .filter(|n| !n.is_empty())
            {
                match slath_db::orgs::resolve_namespace(&self.pool, n).await? {
                    Some((_, _, slath_db::orgs::NamespaceKind::User(u))) => {
                        ids.push(serde_json::Value::String(u.to_string()));
                    }
                    _ => {
                        return Err(ServiceError::invalid(
                            "required_checks",
                            "unknown",
                            format!("{n} isn't a user or bot."),
                        ));
                    }
                }
            }
            if !contexts.iter().any(|c: &String| c == ctx) {
                contexts.push(ctx.to_owned());
            }
            if !ids.is_empty() {
                reporters.insert(ctx.to_owned(), serde_json::Value::Array(ids));
            }
        }
        let reporters = serde_json::Value::Object(reporters);
        let mut bot_mergers: Vec<uuid::Uuid> = Vec::new();
        for n in r
            .bot_merge_allowlist
            .iter()
            .map(|n| n.trim().trim_start_matches('@'))
            .filter(|n| !n.is_empty())
        {
            let Some(bot) = slath_db::bots::find(&self.pool, n).await? else {
                return Err(ServiceError::invalid(
                    "bot_merge_allowlist",
                    "unknown",
                    format!("{n} isn't a bot."),
                ));
            };
            if !bot_mergers.contains(bot.id.as_uuid()) {
                bot_mergers.push(*bot.id.as_uuid());
            }
        }
        slath_db::protections::upsert(
            &self.pool,
            a.repo.id,
            &NewProtection {
                pattern,
                block_force_push: r.block_force_push,
                block_deletion: r.block_deletion,
                require_mr: r.require_mr,
                push_role: &r.push_role,
                required_approvals: r.required_approvals,
                dismiss_stale_approvals: r.dismiss_stale_approvals,
                require_resolved_threads: r.require_resolved_threads,
                require_up_to_date: r.require_up_to_date,
                bots_count_as_approvers: r.bots_count_as_approvers,
                required_status_contexts: &contexts,
                statuses_on_head_ok: r.statuses_on_head_ok,
                status_reporters: &reporters,
                dismiss_approvals_on_new_revision: r.dismiss_approvals_on_new_revision,
                bot_merge_allowlist: &bot_mergers,
            },
        )
        .await?;
        self.audit(
            subject,
            "protection.set",
            crate::audit::Target::repo(*a.repo.id.as_uuid()),
            serde_json::json!({
                "pattern": pattern,
                "block_force_push": r.block_force_push,
                "block_deletion": r.block_deletion,
                "require_mr": r.require_mr,
                "push_role": r.push_role,
                "required_approvals": r.required_approvals,
                "dismiss_stale_approvals": r.dismiss_stale_approvals,
                "require_resolved_threads": r.require_resolved_threads,
                "require_up_to_date": r.require_up_to_date,
                "bots_count_as_approvers": r.bots_count_as_approvers,
                "required_status_contexts": contexts,
                "status_reporters": reporters,
                "statuses_on_head_ok": r.statuses_on_head_ok,
                "dismiss_approvals_on_new_revision": r.dismiss_approvals_on_new_revision,
                "bot_merge_allowlist": bot_mergers,
            }),
        )
        .await;
        Ok(())
    }

    /// A rule's reporter lists with user names instead of IDs.
    pub async fn reporter_names(
        &self,
        p: &Protection,
    ) -> Result<std::collections::BTreeMap<String, Vec<String>>> {
        let mut out = std::collections::BTreeMap::new();
        for (ctx, ids) in reporters_of(p) {
            let mut names = Vec::new();
            for u in ids {
                if let Some(user) = slath_db::users::get(&self.pool, u).await? {
                    names.push(user.name);
                }
            }
            out.insert(ctx, names);
        }
        Ok(out)
    }

    /// The names of the bots a rule lets merge.
    pub async fn bot_merger_names(&self, p: &Protection) -> Result<Vec<String>> {
        let mut names = Vec::new();
        for id in &p.bot_merge_allowlist {
            if let Some(u) = slath_db::users::get(&self.pool, UserId::from_uuid(*id)).await? {
                names.push(u.name);
            }
        }
        names.sort();
        Ok(names)
    }

    /// Removes a rule. Audited.
    pub async fn delete_protection(
        &self,
        subject: &Subject,
        ns: &str,
        repo: &str,
        id: uuid::Uuid,
    ) -> Result<()> {
        let a = self
            .repo_access(subject, ns, repo, Action::ProtectionManage)
            .await?;
        if !slath_db::protections::delete(&self.pool, a.repo.id, id).await? {
            return Err(ServiceError::NotFound);
        }
        self.audit(
            subject,
            "protection.delete",
            crate::audit::Target::repo(*a.repo.id.as_uuid()),
            serde_json::json!({"id": id}),
        )
        .await;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn globs() {
        for (p, n, want) in [
            ("main", "main", true),
            ("main", "mainline", false),
            ("release/*", "release/1.0", true),
            ("release/*", "release/1.0/hotfix", false),
            ("release/*", "release/", true),
            ("*", "feature", true),
            ("*", "feature/x", false),
            ("*/*", "feature/x", true),
            ("v?", "v1", true),
            ("v?", "v10", false),
            ("v?", "v/", false),
            ("*-stable", "1.0-stable", true),
        ] {
            assert_eq!(glob_match(p.as_bytes(), n.as_bytes()), want, "{p} {n}");
        }
    }

    fn rule(pattern: &str, force: bool, del: bool, mr: bool, role: &str) -> Protection {
        Protection {
            id: uuid::Uuid::nil(),
            pattern: pattern.to_owned(),
            block_force_push: force,
            block_deletion: del,
            require_mr: mr,
            push_role: role.to_owned(),
            required_approvals: 1,
            dismiss_stale_approvals: true,
            require_resolved_threads: false,
            require_up_to_date: false,
            bots_count_as_approvers: false,
            required_status_contexts: Vec::new(),
            statuses_on_head_ok: true,
            status_reporters: serde_json::json!({}),
            dismiss_approvals_on_new_revision: false,
            bot_merge_allowlist: Vec::new(),
            created_at: time::OffsetDateTime::UNIX_EPOCH,
        }
    }

    #[test]
    fn bots_merge_only_where_every_rule_lists_them() {
        let (b1, b2) = (uuid::Uuid::from_u128(1), uuid::Uuid::from_u128(2));
        let mut a = rule("main", false, false, false, "write");
        a.bot_merge_allowlist = vec![b1, b2];
        let mut b = rule("*", false, false, false, "write");
        b.bot_merge_allowlist = vec![b2];
        let bot = |id| Subject::User {
            id: UserId::from_uuid(id),
            kind: slath_core::authz::UserKind::Bot,
            state: slath_core::authz::UserState::Active,
            is_site_admin: false,
            credential: slath_core::authz::Credential::Session,
        };
        let e = effective(&[a.clone(), b], b"main");
        assert!(e.denies_bot(&bot(b1)));
        assert!(!e.denies_bot(&bot(b2)));
        assert!(!effective(&[a.clone()], b"main").denies_bot(&bot(b1)));
        // Unprotected branches and people are never affected.
        assert!(!effective(&[a], b"dev").denies_bot(&bot(b1)));
        let person = Subject::User {
            id: UserId::from_uuid(b1),
            kind: slath_core::authz::UserKind::Human,
            state: slath_core::authz::UserState::Active,
            is_site_admin: false,
            credential: slath_core::authz::Credential::Session,
        };
        assert!(
            !effective(&[rule("main", false, false, false, "write")], b"main").denies_bot(&person)
        );
    }

    #[test]
    fn strictest_rule_wins() {
        let rules = vec![
            rule("main", false, true, false, "write"),
            rule("*", true, false, false, "maintain"),
            rule("release/*", false, false, true, "nobody"),
        ];
        let m = effective(&rules, b"main");
        assert!(m.protected && m.block_force_push && m.block_deletion && !m.require_mr);
        assert_eq!(m.push_role, Some(Role::Maintain));
        let r = effective(&rules, b"release/1");
        assert!(r.nobody && r.require_mr);
        assert!(!effective(&rules, b"feature/x").protected);
    }

    #[test]
    fn merge_request_settings_combine_strictest() {
        let mut a = rule("main", false, false, false, "write");
        a.required_approvals = 2;
        a.bots_count_as_approvers = true;
        let mut b = rule("*", false, false, false, "write");
        b.required_approvals = 1;
        b.dismiss_stale_approvals = false;
        b.require_up_to_date = true;
        let e = effective(&[a.clone(), b], b"main");
        assert_eq!(e.required_approvals, 2);
        assert!(e.dismiss_stale_approvals && e.require_up_to_date);
        // Bots count only if every matching rule says so.
        assert!(!e.bots_count_as_approvers);
        assert!(effective(&[a], b"main").bots_count_as_approvers);
        // Unprotected branches require nothing.
        let none = effective(&[], b"main");
        assert_eq!(
            (none.required_approvals, none.bots_count_as_approvers),
            (0, false)
        );
    }

    #[test]
    fn patterns() {
        for ok in ["main", "release/*", "v?.*", "feature/**"] {
            assert!(check_pattern(ok).is_ok(), "{ok}");
        }
        for bad in ["", "/main", "main/", "a//b", "a..b", "a b", "a[b]", "a:b"] {
            assert!(check_pattern(bad).is_err(), "{bad}");
        }
    }
}
