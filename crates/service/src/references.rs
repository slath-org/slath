// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Reference autocomplete for the Markdown editor.
//!
//! Suggestions come only from a repository the subject can read, and only
//! signed-in subjects get them. People are ranked: participants of the
//! current thread, then people with access to the repository, then everyone
//! else. On instances with open registration, a subject without a grant on
//! the repository only gets people who already took part in it, so the
//! endpoint can't be used to list the instance's accounts. Responses never
//! contain email addresses, and each user may ask 60 times a minute.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use slath_core::authz::{Action, Subject};
use slath_core::config::Registration;

use crate::Services;
use crate::error::{Result, ServiceError};

/// Suggestions per answer.
pub const MAX_SUGGESTIONS: i64 = 10;
/// Requests per user and window.
const REQUESTS_PER_WINDOW: u32 = 60;
const WINDOW: Duration = Duration::from_secs(60);
/// Longest query in characters.
const MAX_QUERY: usize = 100;

/// What is being completed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    User,
    Team,
    Item,
    Label,
    Milestone,
}

impl Kind {
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "user" => Self::User,
            "team" => Self::Team,
            "item" => Self::Item,
            "label" => Self::Label,
            "milestone" => Self::Milestone,
            _ => return None,
        })
    }
}

/// One suggestion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Suggestion {
    /// `user`, `team`, `item`, `label`, or `milestone`.
    pub kind: &'static str,
    /// Text to insert, e.g. `@alice`, `#12`, `~"needs review"`.
    pub value: String,
    /// What to show: a name, title, or label.
    pub label: String,
    /// Secondary text: display name, issue state, description.
    pub detail: Option<String>,
    /// Label color (`rrggbb`).
    pub color: Option<String>,
}

/// Fixed-window request counter per user.
#[derive(Debug, Default)]
pub struct Limiter {
    windows: Mutex<HashMap<uuid::Uuid, (Instant, u32)>>,
}

impl Limiter {
    fn take(&self, user: uuid::Uuid) -> Result<()> {
        let now = Instant::now();
        let Ok(mut map) = self.windows.lock() else {
            return Err(ServiceError::Unavailable);
        };
        if map.len() > 100_000 {
            map.retain(|_, (t, _)| now.duration_since(*t) < WINDOW);
        }
        let (start, n) = map.entry(user).or_insert((now, 0));
        if now.duration_since(*start) >= WINDOW {
            *start = now;
            *n = 0;
        }
        *n += 1;
        if *n > REQUESTS_PER_WINDOW {
            let left = WINDOW.saturating_sub(now.duration_since(*start));
            return Err(ServiceError::RateLimited {
                retry_after: left.as_secs().max(1),
            });
        }
        Ok(())
    }
}

fn is_plain(s: &str) -> bool {
    let b = s.as_bytes();
    !b.is_empty()
        && b[0].is_ascii_alphanumeric()
        && b.iter()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'_' | b'-'))
        && !s.ends_with('.')
}

/// `~bug` or `~"needs review"` (`None` if it can't be written).
fn quoted(sigil: char, name: &str) -> Option<String> {
    if is_plain(name) {
        Some(format!("{sigil}{name}"))
    } else if name.contains(['"', '\n', '\r']) {
        None
    } else {
        Some(format!("{sigil}\"{name}\""))
    }
}

impl Services {
    /// Suggestions for completing a reference of `kind` starting with `q` in
    /// a repository's text. `item` is the issue being edited, if any.
    pub async fn reference_suggestions(
        &self,
        subject: &Subject,
        ns: &str,
        repo: &str,
        kind: &str,
        q: &str,
        item: Option<i32>,
    ) -> Result<Vec<Suggestion>> {
        let Subject::User {
            id: user,
            is_site_admin,
            ..
        } = subject
        else {
            return Err(ServiceError::Unauthenticated);
        };
        self.reference_limiter.take(*user.as_uuid())?;
        let a = self
            .repo_access(subject, ns, repo, Action::RepoRead)
            .await?;
        let kind = Kind::parse(kind).ok_or_else(|| {
            ServiceError::invalid(
                "kind",
                "invalid",
                "Use user, team, item, label, or milestone.",
            )
        })?;
        let q: String = q.trim().chars().take(MAX_QUERY).collect();
        let q = match kind {
            Kind::User | Kind::Team => q.trim_start_matches('@').to_owned(),
            Kind::Item => q.trim_start_matches('#').to_owned(),
            Kind::Label => q.trim_start_matches('~').trim_matches('"').to_owned(),
            Kind::Milestone => q.trim_start_matches('%').trim_matches('"').to_owned(),
        };
        let issues = slath_db::repos::issues_enabled(&self.pool, a.repo.id).await?;
        let out = match kind {
            Kind::User => {
                let thread = match item {
                    Some(n) if issues => slath_db::issues::get(&self.pool, a.repo.id, n)
                        .await?
                        .map(|i| i.id),
                    _ => None,
                };
                let participants_only = self.config.auth.registration == Registration::Open
                    && !*is_site_admin
                    && !slath_db::references::has_grant(&self.pool, a.repo.id, *user).await?;
                slath_db::references::users(
                    &self.pool,
                    a.repo.id,
                    thread,
                    &q,
                    participants_only,
                    MAX_SUGGESTIONS,
                )
                .await?
                .into_iter()
                .map(|u| Suggestion {
                    kind: "user",
                    value: format!("@{}", u.name),
                    label: u.name,
                    detail: u.display_name,
                    color: None,
                })
                .collect()
            }
            Kind::Team => {
                slath_db::references::teams(&self.pool, a.repo.id, *user, &q, MAX_SUGGESTIONS)
                    .await?
                    .into_iter()
                    .map(|t| Suggestion {
                        kind: "team",
                        value: format!("@{t}"),
                        label: t,
                        detail: None,
                        color: None,
                    })
                    .collect()
            }
            Kind::Item if issues => {
                slath_db::references::items(&self.pool, a.repo.id, &q, MAX_SUGGESTIONS)
                    .await?
                    .into_iter()
                    .map(|(n, title, state)| Suggestion {
                        kind: "item",
                        value: format!("#{n}"),
                        label: title,
                        detail: Some(state),
                        color: None,
                    })
                    .collect()
            }
            Kind::Item => Vec::new(),
            Kind::Label => slath_db::references::labels(&self.pool, a.repo.id, &q, MAX_SUGGESTIONS)
                .await?
                .into_iter()
                .filter_map(|(name, color, description)| {
                    Some(Suggestion {
                        kind: "label",
                        value: quoted('~', &name)?,
                        label: name,
                        detail: description,
                        color: Some(color),
                    })
                })
                .collect(),
            Kind::Milestone => {
                slath_db::references::milestones(&self.pool, a.repo.id, &q, MAX_SUGGESTIONS)
                    .await?
                    .into_iter()
                    .filter_map(|(title, state)| {
                        Some(Suggestion {
                            kind: "milestone",
                            value: quoted('%', &title)?,
                            label: title,
                            detail: Some(state),
                            color: None,
                        })
                    })
                    .collect()
            }
        };
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quoting() {
        assert_eq!(quoted('~', "bug").as_deref(), Some("~bug"));
        assert_eq!(
            quoted('~', "needs review").as_deref(),
            Some("~\"needs review\"")
        );
        assert_eq!(quoted('%', "v1.0").as_deref(), Some("%v1.0"));
        assert_eq!(quoted('%', "v1.").as_deref(), Some("%\"v1.\""));
        assert_eq!(quoted('~', "a\"b"), None);
    }

    #[test]
    fn the_limiter_allows_60_a_minute() {
        let l = Limiter::default();
        let u = uuid::Uuid::now_v7();
        for _ in 0..60 {
            assert!(l.take(u).is_ok());
        }
        assert!(matches!(l.take(u), Err(ServiceError::RateLimited { .. })));
        assert!(l.take(uuid::Uuid::now_v7()).is_ok());
    }
}
