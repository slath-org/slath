// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! `slath admin apply`: a declarative bootstrap of users, organizations,
//! bots, repositories, and tokens, for scripted and agent setups.
//!
//! Applying is idempotent: what exists is left as it is (missing
//! memberships, collaborators, and tokens are added), so a second run
//! reports no changes. Nothing is ever removed. Objects are created as
//! their owner would create them (repositories by their namespace's owner),
//! and every change is audited.

use std::collections::BTreeMap;

use serde::Deserialize;
use slath_core::Secret;
use slath_core::authz::{Credential, Role, Scope, Subject, UserKind, UserState, Visibility};
use slath_core::credential::CredentialKind;
use slath_core::ids::{RepoId, UserId};
use slath_db::bots::BotOwner;
use slath_db::orgs::{NamespaceKind, OrgRole};
use time::OffsetDateTime;

use crate::Services;
use crate::bots::NewBotToken;
use crate::error::{Result, ServiceError};

/// The bootstrap file (TOML).
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Plan {
    #[serde(default)]
    pub users: Vec<PlanUser>,
    #[serde(default)]
    pub orgs: Vec<PlanOrg>,
    #[serde(default)]
    pub bots: Vec<PlanBot>,
    #[serde(default)]
    pub repos: Vec<PlanRepo>,
    #[serde(default)]
    pub tokens: Vec<PlanToken>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlanUser {
    pub username: String,
    pub email: Option<String>,
    pub display_name: Option<String>,
    #[serde(default)]
    pub admin: bool,
    /// Where the password is read from, or written to (mode 0600) when
    /// the account is created; generated then if the file doesn't exist.
    pub password_file: Option<String>,
    /// Filled in by the caller: the password to use if the account is
    /// created.
    #[serde(skip)]
    pub password: Option<Secret<String>>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlanOrg {
    pub name: String,
    pub display_name: Option<String>,
    /// At least one; the first creates the organization.
    pub owners: Vec<String>,
    #[serde(default)]
    pub members: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlanBot {
    pub name: String,
    /// A user or organization.
    pub owner: String,
    pub display_name: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlanRepo {
    pub namespace: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    /// `public`, `internal`, or `private` (default).
    #[serde(default = "private")]
    pub visibility: String,
    #[serde(default = "main")]
    pub default_branch: String,
    /// Username (person or bot) → role (`read`, `write`, `maintain`, `admin`).
    #[serde(default)]
    pub collaborators: BTreeMap<String, String>,
}

fn private() -> String {
    "private".to_owned()
}

fn main() -> String {
    "main".to_owned()
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlanToken {
    /// Names the token; one active token per account and name.
    pub name: String,
    /// The account: a person (`user`) or a bot (`bot`), exactly one.
    pub user: Option<String>,
    pub bot: Option<String>,
    pub scopes: Vec<String>,
    /// `namespace/name`; required for bots, optional for people.
    #[serde(default)]
    pub repos: Vec<String>,
    /// Days until it expires (bots: default and maximum from `[auth]`;
    /// people: never without).
    pub days: Option<u32>,
    /// Where the token is written (mode 0600).
    pub file: String,
    /// Filled in by the caller: whether `file` exists. A token is created
    /// again when its file is missing.
    #[serde(skip)]
    pub file_present: bool,
}

/// One thing applying changed.
#[derive(Debug)]
pub struct Change {
    /// `user`, `org`, `org_member`, `bot`, `repo`, `collaborator`, `token`.
    pub kind: &'static str,
    pub name: String,
    /// `created` or `updated`.
    pub action: &'static str,
    /// A new token, for the caller to write to `file`.
    pub secret: Option<Secret<String>>,
    /// The file `secret` (or a generated password) belongs in.
    pub file: Option<String>,
}

impl Change {
    fn new(kind: &'static str, name: impl Into<String>, action: &'static str) -> Self {
        Self {
            kind,
            name: name.into(),
            action,
            secret: None,
            file: None,
        }
    }
}

fn invalid(what: &str, message: impl Into<String>) -> ServiceError {
    ServiceError::invalid(what, "invalid", message)
}

fn role(s: &str) -> Result<Role> {
    s.parse()
        .map_err(|_| invalid("collaborators", format!("Unknown role {s:?}.")))
}

impl Services {
    /// A person acting through this command, as if signed in.
    async fn acting_as(&self, user: UserId) -> Result<Subject> {
        let u = slath_db::users::get(&self.pool, user)
            .await?
            .ok_or(ServiceError::NotFound)?;
        if u.kind != UserKind::Human || u.state != UserState::Active {
            return Err(invalid(
                "owner",
                format!("{} isn't an active person.", u.name),
            ));
        }
        Ok(Subject::User {
            id: u.id,
            kind: UserKind::Human,
            state: UserState::Active,
            is_site_admin: u.is_admin,
            credential: Credential::Session,
        })
    }

    async fn person(&self, name: &str) -> Result<UserId> {
        match slath_db::orgs::resolve_namespace(&self.pool, name.trim()).await? {
            Some((_, _, NamespaceKind::User(u))) => Ok(u),
            _ => Err(invalid("users", format!("There's no user {name:?}."))),
        }
    }

    /// Applies a bootstrap plan (the system acting). Stops at the first
    /// error; what was applied before it stays.
    pub async fn apply(&self, plan: Plan) -> Result<Vec<Change>> {
        let mut changes = Vec::new();
        for u in plan.users {
            if slath_db::users::get_by_name(&self.pool, u.username.trim())
                .await?
                .is_some()
            {
                continue;
            }
            let generated = u.password.is_none();
            let password = match u.password {
                Some(p) => p,
                None => Secret::new(random_password()?),
            };
            self.create_account(
                &Subject::System,
                crate::accounts::NewAccount {
                    username: u.username.clone(),
                    email: u.email.clone(),
                    display_name: u.display_name.clone(),
                    password: Some(Secret::new(password.expose().clone())),
                    is_admin: u.admin,
                },
            )
            .await?;
            let mut c = Change::new("user", u.username.trim(), "created");
            if generated {
                c.secret = Some(password);
                c.file = u.password_file.clone();
            }
            changes.push(c);
        }

        for o in plan.orgs {
            let Some(first) = o.owners.first() else {
                return Err(invalid("orgs", format!("{} needs an owner.", o.name)));
            };
            let first = self.person(first).await?;
            let org = match slath_db::orgs::resolve_namespace(&self.pool, o.name.trim()).await? {
                Some((_, _, NamespaceKind::Org(id))) => id,
                Some(_) => {
                    return Err(invalid("orgs", format!("{} is a user.", o.name)));
                }
                None => {
                    let mut tx = self.pool.begin().await.map_err(ServiceError::internal)?;
                    let id = slath_db::orgs::create(
                        &mut tx,
                        o.name.trim(),
                        o.display_name.as_deref(),
                        first,
                    )
                    .await?;
                    tx.commit().await.map_err(ServiceError::internal)?;
                    self.audit(
                        &self.acting_as(first).await?,
                        "org.create",
                        crate::audit::Target::org(*id.as_uuid()),
                        serde_json::json!({"name": o.name.trim(), "via": "apply"}),
                    )
                    .await;
                    changes.push(Change::new("org", o.name.trim(), "created"));
                    id
                }
            };
            let owner = self.acting_as(first).await?;
            let wanted = o
                .owners
                .iter()
                .map(|n| (n, OrgRole::Owner))
                .chain(o.members.iter().map(|n| (n, OrgRole::Member)));
            for (name, role) in wanted {
                let user = self.person(name).await?;
                if slath_db::orgs::role_of(&self.pool, org, user)
                    .await?
                    .is_some()
                {
                    continue;
                }
                self.set_org_member(&owner, org, name, role).await?;
                changes.push(Change::new(
                    "org_member",
                    format!("{}/{}", o.name.trim(), name.trim()),
                    "created",
                ));
            }
        }

        for b in plan.bots {
            let owner = match slath_db::orgs::resolve_namespace(&self.pool, b.owner.trim()).await? {
                Some((_, _, NamespaceKind::Org(o))) => BotOwner::Org(o),
                Some((_, _, NamespaceKind::User(u))) => {
                    self.acting_as(u).await?;
                    BotOwner::User(u)
                }
                None => {
                    return Err(invalid("bots", format!("There's no owner {:?}.", b.owner)));
                }
            };
            match slath_db::bots::find(&self.pool, b.name.trim()).await? {
                Some(existing) if existing.owner == owner => {}
                Some(_) => {
                    return Err(invalid(
                        "bots",
                        format!("{} belongs to someone else.", b.name.trim()),
                    ));
                }
                None => {
                    self.insert_bot(
                        &Subject::System,
                        owner,
                        &b.name,
                        b.display_name.as_deref().unwrap_or(""),
                    )
                    .await?;
                    changes.push(Change::new("bot", b.name.trim(), "created"));
                }
            }
        }

        for r in plan.repos {
            let full = format!("{}/{}", r.namespace.trim(), r.name.trim());
            // The namespace's owner creates it: the person, or an
            // organization's first owner.
            let creator = match slath_db::orgs::resolve_namespace(&self.pool, r.namespace.trim())
                .await?
            {
                Some((_, _, NamespaceKind::User(u))) => u,
                Some((_, _, NamespaceKind::Org(o))) => {
                    slath_db::orgs::members(&self.pool, o)
                        .await?
                        .into_iter()
                        .find(|m| m.role == OrgRole::Owner)
                        .ok_or_else(|| invalid("repos", format!("{} has no owner.", r.namespace)))?
                        .user_id
                }
                None => {
                    return Err(invalid(
                        "repos",
                        format!("There's no namespace {:?}.", r.namespace),
                    ));
                }
            };
            let subject = self.acting_as(creator).await?;
            let access = match slath_db::repos::find(&self.pool, r.namespace.trim(), r.name.trim())
                .await?
            {
                Some(repo) => self.access_for_apply(&subject, repo).await?,
                None => {
                    let visibility = match r.visibility.as_str() {
                        "public" => Visibility::Public,
                        "internal" => Visibility::Internal,
                        "private" => Visibility::Private,
                        v => return Err(invalid("repos", format!("Unknown visibility {v:?}."))),
                    };
                    let a = self
                        .create_repo(
                            &subject,
                            crate::repos::NewRepoInput {
                                namespace: r.namespace.trim().to_owned(),
                                name: r.name.trim().to_owned(),
                                description: r.description.clone(),
                                visibility,
                                default_branch: r.default_branch.clone(),
                            },
                        )
                        .await?;
                    changes.push(Change::new("repo", full.clone(), "created"));
                    a
                }
            };
            let current = slath_db::repos::collaborators(&self.pool, access.repo.id).await?;
            for (name, wanted) in &r.collaborators {
                let wanted = role(wanted)?;
                let have = current
                    .iter()
                    .find(|c| c.name.eq_ignore_ascii_case(name.trim()))
                    .map(|c| c.role);
                if have == Some(wanted) {
                    continue;
                }
                self.set_collaborator(&subject, &access, name, wanted)
                    .await?;
                changes.push(Change::new(
                    "collaborator",
                    format!("{full} {}", name.trim()),
                    if have.is_some() { "updated" } else { "created" },
                ));
            }
        }

        for t in plan.tokens {
            changes.push(match self.apply_token(t).await? {
                Some(c) => c,
                None => continue,
            });
        }

        if !changes.is_empty() {
            self.audit(
                &Subject::System,
                "admin.apply",
                crate::audit::Target::new("instance", None),
                serde_json::json!({"changes": changes.len()}),
            )
            .await;
        }
        Ok(changes)
    }

    async fn access_for_apply(
        &self,
        subject: &Subject,
        repo: slath_db::repos::Repo,
    ) -> Result<crate::repos::RepoAccess> {
        self.repo_access(
            subject,
            &repo.namespace,
            &repo.name,
            slath_core::authz::Action::RepoRead,
        )
        .await
    }

    /// Creates a token unless an active one of that name exists and its
    /// file is there; a missing file replaces the token (the old one is
    /// revoked).
    async fn apply_token(&self, t: PlanToken) -> Result<Option<Change>> {
        let (account, bot) = match (&t.user, &t.bot) {
            (Some(u), None) => (self.person(u).await?, None),
            (None, Some(b)) => {
                let bot = slath_db::bots::find(&self.pool, b.trim())
                    .await?
                    .ok_or_else(|| invalid("tokens", format!("There's no bot {b:?}.")))?;
                (bot.id, Some(bot))
            }
            _ => {
                return Err(invalid(
                    "tokens",
                    format!("Token {:?} needs exactly one of user or bot.", t.name),
                ));
            }
        };
        let now = OffsetDateTime::now_utc();
        let active: Vec<_> = slath_db::tokens::list_for_user(&self.pool, account)
            .await?
            .into_iter()
            .filter(|r| {
                r.name == t.name.trim()
                    && r.revoked_at.is_none()
                    && r.expires_at.is_none_or(|e| e > now)
            })
            .collect();
        if !active.is_empty() && t.file_present {
            return Ok(None);
        }
        let label = format!(
            "{} {}",
            t.user
                .as_deref()
                .or(t.bot.as_deref())
                .unwrap_or_default()
                .trim(),
            t.name.trim()
        );
        let (secret, _) = match &bot {
            Some(bot) => {
                let checked = self
                    .check_bot_token(
                        &Subject::System,
                        &NewBotToken {
                            name: t.name.clone(),
                            scopes: t.scopes.clone(),
                            repos: t.repos.clone(),
                            expires_in_days: t.days,
                        },
                    )
                    .await?;
                for r in &active {
                    self.revoke_token_of(&Subject::System, account, r.id)
                        .await?;
                }
                self.store_bot_token(&Subject::System, bot, checked).await?
            }
            None => {
                let (scopes, repos) = self.check_person_token(account, &t).await?;
                for r in &active {
                    self.revoke_token_of(&Subject::System, account, r.id)
                        .await?;
                }
                self.store_token(
                    &Subject::System,
                    account,
                    CredentialKind::PersonalAccessToken,
                    t.name.trim(),
                    scopes,
                    repos.as_deref(),
                    t.days
                        .map(|d| now + time::Duration::days(i64::from(d.max(1)))),
                )
                .await?
            }
        };
        let mut c = Change::new(
            "token",
            label,
            if active.is_empty() {
                "created"
            } else {
                "updated"
            },
        );
        c.secret = Some(secret);
        c.file = Some(t.file);
        Ok(Some(c))
    }

    async fn check_person_token(
        &self,
        user: UserId,
        t: &PlanToken,
    ) -> Result<(Vec<Scope>, Option<Vec<RepoId>>)> {
        let is_admin = slath_db::users::get(&self.pool, user)
            .await?
            .is_some_and(|u| u.is_admin);
        let mut scopes = Vec::new();
        for raw in &t.scopes {
            let s: Scope = raw
                .trim()
                .parse()
                .map_err(|_| invalid("tokens", format!("Unknown scope {raw:?}.")))?;
            if s == Scope::Admin && !is_admin {
                return Err(invalid(
                    "tokens",
                    "Only site administrators can have admin tokens.",
                ));
            }
            if !scopes.contains(&s) {
                scopes.push(s);
            }
        }
        if scopes.is_empty() {
            return Err(invalid(
                "tokens",
                format!("Token {:?} needs scopes.", t.name),
            ));
        }
        if t.repos.is_empty() {
            return Ok((scopes, None));
        }
        let mut repos = Vec::new();
        for full in &t.repos {
            let (ns, name) = full
                .split_once('/')
                .ok_or_else(|| invalid("tokens", format!("{full:?} isn't namespace/name.")))?;
            let repo = slath_db::repos::find(&self.pool, ns.trim(), name.trim())
                .await?
                .ok_or_else(|| invalid("tokens", format!("There's no repository {full:?}.")))?;
            repos.push(repo.id);
        }
        Ok((scopes, Some(repos)))
    }
}

/// A random password: 24 characters from an unambiguous alphabet.
fn random_password() -> Result<String> {
    const ALPHABET: &[u8] = b"abcdefghijkmnopqrstuvwxyzABCDEFGHJKLMNPQRSTUVWXYZ23456789";
    let bytes = slath_auth::token::random_bytes::<24>().map_err(ServiceError::internal)?;
    Ok(bytes
        .iter()
        .map(|b| ALPHABET[usize::from(*b) % ALPHABET.len()] as char)
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plans_parse_and_reject_unknown_keys() {
        let p: Plan = toml::from_str(
            r#"
            [[users]]
            username = "admin"
            admin = true
            password_file = "secrets/admin-password"

            [[orgs]]
            name = "acme"
            owners = ["admin"]

            [[bots]]
            name = "acme-agent"
            owner = "acme"

            [[repos]]
            namespace = "acme"
            name = "app"
            collaborators = { "acme-agent" = "write" }

            [[tokens]]
            name = "agent"
            bot = "acme-agent"
            scopes = ["repo:write", "mrs:write"]
            repos = ["acme/app"]
            file = "secrets/bot-token"
            "#,
        )
        .unwrap();
        assert_eq!(p.repos[0].visibility, "private");
        assert_eq!(p.repos[0].default_branch, "main");
        assert_eq!(p.tokens[0].bot.as_deref(), Some("acme-agent"));
        assert!(toml::from_str::<Plan>("[[users]]\nusername = \"x\"\npassword = \"y\"\n").is_err());
        assert!(toml::from_str::<Plan>("[[teams]]\nname = \"x\"\n").is_err());
    }
}
