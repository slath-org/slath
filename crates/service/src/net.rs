// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Outbound connections to user-supplied URLs: URL checks, name
//! resolution, and the address policy ([`slath_core::net`]).
//!
//! Every address a name resolves to must be allowed (one bad answer
//! rejects the name, which defeats multi-answer tricks), and the caller
//! connects only to the checked address it gets back, so a second
//! resolution (DNS rebinding) can't send it elsewhere.

use std::net::IpAddr;

use slath_core::net::{Cidr, is_denied};

/// A checked destination.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Destination {
    /// The URL without credentials, fragment, or query.
    pub url: url::Url,
    /// Host as written in the URL (lowercase, IDNA-encoded).
    pub host: String,
    pub port: u16,
    /// The address to connect to.
    pub ip: IpAddr,
}

/// Why a URL is refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum UrlError {
    #[error("Enter an https:// URL.")]
    Scheme,
    #[error("This URL is not valid.")]
    Invalid,
    #[error("Put credentials in the username and password fields, not in the URL.")]
    Credentials,
    #[error("This address can't be reached from here (internal or reserved network).")]
    Forbidden,
    #[error("The server name doesn't resolve.")]
    Unresolvable,
}

/// Parses and checks a URL's syntax: `https` (or `http` when allowed), a
/// host, no credentials, no control characters or whitespace, and nothing
/// Git could take for an option or a local path.
pub fn parse(raw: &str, allow_http: bool) -> Result<url::Url, UrlError> {
    if raw.is_empty()
        || raw.len() > 2048
        || raw.starts_with('-')
        || raw.chars().any(|c| c.is_control() || c.is_whitespace())
    {
        return Err(UrlError::Invalid);
    }
    let lower = raw.to_ascii_lowercase();
    let scheme_ok = lower.starts_with("https://") || (allow_http && lower.starts_with("http://"));
    if !scheme_ok {
        return Err(UrlError::Scheme);
    }
    let mut url = url::Url::parse(raw).map_err(|_| UrlError::Invalid)?;
    if !(url.scheme() == "https" || (allow_http && url.scheme() == "http")) {
        return Err(UrlError::Scheme);
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(UrlError::Credentials);
    }
    if url.host().is_none() || url.cannot_be_a_base() {
        return Err(UrlError::Invalid);
    }
    url.set_fragment(None);
    url.set_query(None);
    Ok(url)
}

async fn system_lookup(host: String, port: u16) -> Vec<IpAddr> {
    tokio::net::lookup_host((host.as_str(), port))
        .await
        .map(|it| it.map(|a| a.ip()).collect())
        .unwrap_or_default()
}

/// Resolves the URL's host and checks every address. `own_host` is the
/// instance's own host name, which is always refused.
pub async fn resolve(
    url: &url::Url,
    allowed: &[Cidr],
    own_host: &str,
) -> Result<Destination, UrlError> {
    resolve_with(url, allowed, own_host, system_lookup).await
}

/// [`resolve`] with a given name lookup (tests use a fake one).
pub async fn resolve_with<F, Fut>(
    url: &url::Url,
    allowed: &[Cidr],
    own_host: &str,
    lookup: F,
) -> Result<Destination, UrlError>
where
    F: FnOnce(String, u16) -> Fut,
    Fut: Future<Output = Vec<IpAddr>>,
{
    let port = url.port_or_known_default().ok_or(UrlError::Invalid)?;
    let host = url.host_str().ok_or(UrlError::Invalid)?.to_owned();
    let bare = host.trim_start_matches('[').trim_end_matches(']');
    // The instance itself is refused unless an administrator explicitly
    // allowed its address.
    let allowed_literal = bare
        .parse::<IpAddr>()
        .is_ok_and(|ip| allowed.iter().any(|c| c.contains(ip)));
    if bare.trim_end_matches('.').eq_ignore_ascii_case(own_host) && !allowed_literal {
        return Err(UrlError::Forbidden);
    }
    let addrs: Vec<IpAddr> = match bare.parse::<IpAddr>() {
        Ok(ip) => vec![ip],
        Err(_) => lookup(bare.to_owned(), port).await,
    };
    check(&addrs, allowed)?;
    let ip = *addrs.first().ok_or(UrlError::Unresolvable)?;
    Ok(Destination {
        url: url.clone(),
        host,
        port,
        ip,
    })
}

/// Fails unless there is at least one address and every one is allowed.
pub fn check(addrs: &[IpAddr], allowed: &[Cidr]) -> Result<(), UrlError> {
    if addrs.is_empty() {
        return Err(UrlError::Unresolvable);
    }
    if addrs.iter().any(|ip| is_denied(*ip, allowed)) {
        return Err(UrlError::Forbidden);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls_are_checked() {
        for bad in [
            "file:///etc/passwd",
            "ssh://host/x.git",
            "git://host/x.git",
            "ext::sh -c touch% /tmp/x",
            "fd::17",
            "--upload-pack=touch /tmp/x",
            "-uhttps://x",
            "/srv/slath/repos/aa/x.git",
            "https://h/x\r",
            "https://h/x\n[core]\nsshCommand=touch /tmp/x",
            "https://h/x y",
            "http://example.org/x.git",
            "HTTPS:/x",
            "https://",
        ] {
            assert!(parse(bad, false).is_err(), "{bad:?}");
        }
        assert_eq!(
            parse("https://user:pw@example.org/x.git", false),
            Err(UrlError::Credentials)
        );
        let u = parse("https://Example.ORG/a/b.git?x=1#frag", false).unwrap();
        assert_eq!(u.as_str(), "https://example.org/a/b.git");
        assert!(parse("http://example.org/x.git", true).is_ok());
    }

    #[tokio::test]
    async fn literal_and_local_hosts_are_refused() {
        for host in [
            "127.0.0.1",
            "127.1",
            "0x7f000001",
            "2130706433",
            "0.0.0.0",
            "[::]",
            "[::1]",
            "[::ffff:127.0.0.1]",
            "[::ffff:7f00:1]",
            "[64:ff9b::127.0.0.1]",
            "10.0.0.1",
            "172.16.0.1",
            "192.168.0.1",
            "100.64.0.1",
            "169.254.169.254",
            "[fd00::1]",
            "224.0.0.1",
            "localhost",
            "localhost.",
            "git.example.org",
        ] {
            let url = parse(&format!("https://{host}/x.git"), false).unwrap();
            let r = resolve(&url, &[], "git.example.org").await;
            assert!(
                matches!(r, Err(UrlError::Forbidden | UrlError::Unresolvable)),
                "{host}: {r:?}"
            );
        }
        let url = parse("https://127.0.0.1:8443/x.git", false).unwrap();
        let ok = resolve(&url, &["127.0.0.0/8".parse().unwrap()], "h")
            .await
            .unwrap();
        assert_eq!(ok.port, 8443);
    }

    #[tokio::test]
    async fn names_resolving_to_internal_addresses_are_refused() {
        let url = parse("https://intranet.example.com/x.git", false).unwrap();
        let fake = |answers: &'static [&'static str]| {
            move |_: String, _: u16| async move {
                answers
                    .iter()
                    .map(|a| a.parse().unwrap())
                    .collect::<Vec<IpAddr>>()
            }
        };
        assert_eq!(
            resolve_with(&url, &[], "h", fake(&["10.0.0.1"])).await,
            Err(UrlError::Forbidden)
        );
        // Multi-answer: one internal address is enough to refuse.
        assert_eq!(
            resolve_with(&url, &[], "h", fake(&["93.184.216.34", "127.0.0.1"])).await,
            Err(UrlError::Forbidden)
        );
        assert_eq!(
            resolve_with(&url, &[], "h", fake(&[])).await,
            Err(UrlError::Unresolvable)
        );
        let ok = resolve_with(&url, &[], "h", fake(&["93.184.216.34"]))
            .await
            .unwrap();
        assert_eq!(ok.ip.to_string(), "93.184.216.34");
        assert_eq!(ok.port, 443);
    }

    #[test]
    fn one_bad_answer_rejects_the_name() {
        let public: IpAddr = "93.184.216.34".parse().unwrap();
        let local: IpAddr = "127.0.0.1".parse().unwrap();
        assert!(check(&[public], &[]).is_ok());
        assert_eq!(check(&[public, local], &[]), Err(UrlError::Forbidden));
        assert_eq!(check(&[], &[]), Err(UrlError::Unresolvable));
    }
}
