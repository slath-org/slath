// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! The opt-in update check: asks the release feed whether a newer release
//! exists and remembers the answer for the administration pages. It never
//! downloads or installs anything, and makes no request unless
//! `[upgrade] check_for_updates` is on.

use std::time::Duration;

use serde::{Deserialize, Serialize};
use slath_core::authz::Subject;
use time::OffsetDateTime;

use crate::Services;
use crate::error::{Result, ServiceError};

/// Key in `instance_settings`.
const SETTING: &str = "update_check";
const TIMEOUT: Duration = Duration::from_secs(10);
const MAX_RESPONSE: u64 = 1024 * 1024;

/// What the last check found.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct UpdateCheck {
    /// When it ran (Unix seconds).
    pub checked_at: i64,
    /// The newest release newer than this one, if any.
    pub available: Option<Release>,
    /// Why the check failed, if it did.
    pub error: Option<String>,
}

impl UpdateCheck {
    #[must_use]
    pub fn checked_at_time(&self) -> OffsetDateTime {
        OffsetDateTime::from_unix_timestamp(self.checked_at).unwrap_or(OffsetDateTime::UNIX_EPOCH)
    }
}

/// A release from the feed.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Release {
    pub version: String,
    /// Its release notes: an `https:` URL, or empty.
    pub url: String,
}

/// One entry of the feed (the GitHub releases format).
#[derive(Debug, Deserialize)]
struct FeedEntry {
    tag_name: String,
    #[serde(default)]
    html_url: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
}

/// `major.minor.patch` of a version or tag (`v1.2.3`); `None` for a
/// pre-release or anything else.
fn stable(v: &str) -> Option<(u64, u64, u64)> {
    let v = v.strip_prefix('v').unwrap_or(v);
    let mut parts = v.split('.');
    let mut next = || parts.next()?.parse::<u64>().ok();
    let out = (next()?, next()?, next()?);
    parts.next().is_none().then_some(out)
}

/// The newest stable release in `feed` newer than `current`.
fn newest_after(feed: Vec<FeedEntry>, current: &str) -> Option<Release> {
    let me = current.strip_prefix('v').unwrap_or(current);
    let me = stable(me.split('-').next().unwrap_or(me))?;
    feed.into_iter()
        .filter(|e| !e.draft && !e.prerelease)
        .filter_map(|e| stable(&e.tag_name).map(|v| (v, e)))
        .filter(|(v, _)| *v > me)
        .max_by_key(|(v, _)| *v)
        .map(|((a, b, c), e)| Release {
            version: format!("{a}.{b}.{c}"),
            // The feed is untrusted: no other schemes reach a link.
            url: url::Url::parse(&e.html_url)
                .ok()
                .filter(|u| u.scheme() == "https")
                .map(String::from)
                .unwrap_or_default(),
        })
}

/// HTTPS, or HTTP to a loopback host (tests).
fn feed_url_ok(url: &str) -> bool {
    url::Url::parse(url).is_ok_and(|u| match u.scheme() {
        "https" => true,
        "http" => matches!(u.host_str(), Some("127.0.0.1" | "localhost" | "[::1]")),
        _ => false,
    })
}

fn fetch(url: &str) -> std::result::Result<Vec<FeedEntry>, String> {
    let agent = ureq::Agent::config_builder()
        .max_redirects(0)
        .timeout_global(Some(TIMEOUT))
        .http_status_as_error(false)
        .user_agent(concat!(
            "Slath/",
            env!("CARGO_PKG_VERSION"),
            " (update check)"
        ))
        .build()
        .new_agent();
    let mut res = agent
        .get(url)
        .header("accept", "application/json")
        .call()
        .map_err(|e| e.to_string())?;
    let status = res.status();
    if !status.is_success() {
        return Err(format!("the release feed answered HTTP {status}"));
    }
    let body = res
        .body_mut()
        .with_config()
        .limit(MAX_RESPONSE)
        .read_to_string()
        .map_err(|e| e.to_string())?;
    serde_json::from_str(&body).map_err(|e| format!("the release feed isn't valid: {e}"))
}

impl Services {
    /// Asks the release feed for a newer release and stores the answer.
    /// Does nothing (and makes no request) unless the check is enabled.
    pub async fn check_for_updates(&self) -> Result<Option<UpdateCheck>> {
        let cfg = &self.config.upgrade;
        if !cfg.check_for_updates {
            return Ok(None);
        }
        let url = cfg.feed_url.clone();
        let result = if feed_url_ok(&url) {
            tokio::task::spawn_blocking(move || fetch(&url))
                .await
                .map_err(ServiceError::internal)?
        } else {
            Err("upgrade.feed_url must be an https:// URL".to_owned())
        };
        let check = match result {
            Ok(feed) => UpdateCheck {
                checked_at: OffsetDateTime::now_utc().unix_timestamp(),
                available: newest_after(feed, env!("CARGO_PKG_VERSION")),
                error: None,
            },
            Err(e) => UpdateCheck {
                checked_at: OffsetDateTime::now_utc().unix_timestamp(),
                available: None,
                error: Some(e),
            },
        };
        let value = serde_json::to_value(&check).map_err(ServiceError::internal)?;
        slath_db::settings::set(&self.pool, SETTING, &value).await?;
        Ok(Some(check))
    }

    /// The last update check, for site administrators; `None` when the
    /// check is off or hasn't run yet.
    pub async fn update_status(&self, subject: &Subject) -> Result<Option<UpdateCheck>> {
        self.require_site_admin(subject)?;
        if !self.config.upgrade.check_for_updates {
            return Ok(None);
        }
        Ok(slath_db::settings::get(&self.pool, SETTING)
            .await?
            .and_then(|v| serde_json::from_value(v).ok()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(tag: &str, draft: bool, prerelease: bool) -> FeedEntry {
        FeedEntry {
            tag_name: tag.to_owned(),
            html_url: format!("https://example.org/{tag}"),
            draft,
            prerelease,
        }
    }

    #[test]
    fn picks_the_newest_stable_release_after_the_current_one() {
        let feed = || {
            vec![
                entry("v0.3.1", false, false),
                entry("v0.10.0", false, false),
                entry("v0.11.0", true, false),
                entry("v0.12.0-rc.1", false, true),
                entry("v0.12.0-rc.2", false, false),
                entry("nonsense", false, false),
                entry("v0.2.0", false, false),
            ]
        };
        assert_eq!(
            newest_after(feed(), "0.3.0"),
            Some(Release {
                version: "0.10.0".into(),
                url: "https://example.org/v0.10.0".into()
            })
        );
        assert_eq!(newest_after(feed(), "0.10.0"), None);
        assert_eq!(newest_after(feed(), "0.10.0-dev").map(|r| r.version), None);
        assert_eq!(newest_after(vec![], "0.3.0"), None);
        let mut hostile = entry("v1.0.0", false, false);
        hostile.html_url = "javascript:alert(1)".into();
        assert_eq!(newest_after(vec![hostile], "0.3.0").unwrap().url, "");
    }

    #[test]
    fn only_https_feeds_except_on_loopback() {
        assert!(feed_url_ok("https://api.github.com/repos/x/y/releases"));
        assert!(feed_url_ok("http://127.0.0.1:9/feed"));
        assert!(!feed_url_ok("http://example.org/feed"));
        assert!(!feed_url_ok("file:///etc/passwd"));
        assert!(!feed_url_ok("not a url"));
    }
}
