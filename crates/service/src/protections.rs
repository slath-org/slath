// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Branch protection: managing the rules (repository admins) and what they
//! mean for a push (pre-receive step 4).
//!
//! A rule's pattern is a glob on the short branch name: `*` matches any
//! characters except `/`, `?` one such character. When several rules match
//! a branch, the strictest value of each setting applies.

use slath_core::authz::{Action, Role, Subject};
use slath_db::protections::{NewProtection, Protection};

use crate::Services;
use crate::error::{Result, ServiceError};

/// The settings that apply to one branch (all matching rules combined).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Effective {
    pub protected: bool,
    pub block_force_push: bool,
    pub block_deletion: bool,
    pub require_mr: bool,
    /// Minimum role for direct pushes; `None` with `protected` means nobody.
    pub push_role: Option<Role>,
    pub nobody: bool,
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
    let mut e = Effective::default();
    for r in rules
        .iter()
        .filter(|r| glob_match(r.pattern.as_bytes(), branch))
    {
        e.protected = true;
        e.block_force_push |= r.block_force_push;
        e.block_deletion |= r.block_deletion;
        e.require_mr |= r.require_mr;
        match push_role(&r.push_role) {
            Some(role) => e.push_role = Some(e.push_role.map_or(role, |p| p.max(role))),
            None => e.nobody = true,
        }
    }
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
        slath_db::protections::upsert(
            &self.pool,
            a.repo.id,
            &NewProtection {
                pattern,
                block_force_push: r.block_force_push,
                block_deletion: r.block_deletion,
                require_mr: r.require_mr,
                push_role: &r.push_role,
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
            }),
        )
        .await;
        Ok(())
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
            created_at: time::OffsetDateTime::UNIX_EPOCH,
        }
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
    fn patterns() {
        for ok in ["main", "release/*", "v?.*", "feature/**"] {
            assert!(check_pattern(ok).is_ok(), "{ok}");
        }
        for bad in ["", "/main", "main/", "a//b", "a..b", "a b", "a[b]", "a:b"] {
            assert!(check_pattern(bad).is_err(), "{bad}");
        }
    }
}
