// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Browser authentication: the session cookie, the client IP, CSRF
//! protection, and safe redirect targets.
//!
//! The session cookie is `__Host-slath_session` (`Secure`) when `public_url`
//! is https. Plain-http development instances use `slath_session`, because
//! browsers refuse `Secure` cookies over http.

use std::net::{IpAddr, SocketAddr};

use axum::body::Body;
use axum::extract::{ConnectInfo, FromRequestParts, Request, State};
use axum::http::request::Parts;
use axum::http::{HeaderMap, HeaderValue, Method, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use base64::Engine;
use slath_core::authz::Subject;
use slath_service::sessions::{NewSessionInfo, Session};

use crate::AppState;
use crate::error::AppError;
use crate::ui::cookie;

/// Name of the session cookie for this instance.
#[must_use]
pub fn session_cookie_name(state: &AppState) -> &'static str {
    if state.config.server.public_url.is_https() {
        "__Host-slath_session"
    } else {
        "slath_session"
    }
}

/// Name of the pre-session CSRF seed cookie (before sign-in).
#[must_use]
pub fn seed_cookie_name(state: &AppState) -> &'static str {
    if state.config.server.public_url.is_https() {
        "__Host-slath_csrf"
    } else {
        "slath_csrf"
    }
}

/// A `Set-Cookie` value with Slath's attributes. `max_age: None` deletes.
#[must_use]
pub fn set_cookie(state: &AppState, name: &str, value: &str, max_age: Option<i64>) -> HeaderValue {
    let secure = if state.config.server.public_url.is_https() {
        "; Secure"
    } else {
        ""
    };
    let age = max_age.unwrap_or(0);
    let v = format!("{name}={value}; Path=/; HttpOnly; SameSite=Lax; Max-Age={age}{secure}");
    HeaderValue::from_str(&v).unwrap_or_else(|_| HeaderValue::from_static("invalid=; Max-Age=0"))
}

/// IP networks from `server.trusted_proxies`.
#[derive(Debug, Clone, Default)]
pub struct TrustedProxies(Vec<(IpAddr, u8)>);

impl TrustedProxies {
    /// Parses `addr/len` entries (a bare address means a single host).
    pub fn parse(list: &[String]) -> Result<Self, String> {
        let mut nets = Vec::new();
        for item in list {
            let (addr, len) = match item.split_once('/') {
                Some((a, l)) => (a, Some(l)),
                None => (item.as_str(), None),
            };
            let ip: IpAddr = addr
                .trim()
                .parse()
                .map_err(|_| format!("server.trusted_proxies: invalid address {item:?}"))?;
            let max = if ip.is_ipv4() { 32 } else { 128 };
            let len = match len {
                Some(l) => l
                    .trim()
                    .parse::<u8>()
                    .ok()
                    .filter(|l| *l <= max)
                    .ok_or_else(|| {
                        format!("server.trusted_proxies: invalid prefix length in {item:?}")
                    })?,
                None => max,
            };
            nets.push((ip, len));
        }
        Ok(Self(nets))
    }

    #[must_use]
    pub fn contains(&self, ip: IpAddr) -> bool {
        let ip = canonical(ip);
        self.0.iter().any(|(net, len)| match (canonical(*net), ip) {
            (IpAddr::V4(n), IpAddr::V4(a)) => prefix_eq(&n.octets(), &a.octets(), *len),
            (IpAddr::V6(n), IpAddr::V6(a)) => prefix_eq(&n.octets(), &a.octets(), *len),
            _ => false,
        })
    }
}

fn canonical(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(v6) => v6.to_ipv4_mapped().map_or(ip, IpAddr::V4),
        v4 => v4,
    }
}

fn prefix_eq(a: &[u8], b: &[u8], len: u8) -> bool {
    let full = usize::from(len / 8);
    let rest = len % 8;
    a[..full] == b[..full] && (rest == 0 || (a[full] ^ b[full]) >> (8 - rest) == 0)
}

/// The client's IP: the socket peer, or the last untrusted address in
/// `X-Forwarded-For` when the peer is a trusted proxy.
#[derive(Debug, Clone, Copy)]
pub struct ClientIp(pub Option<IpAddr>);

impl ClientIp {
    #[must_use]
    pub fn from_parts(
        state: &AppState,
        extensions: &axum::http::Extensions,
        headers: &HeaderMap,
    ) -> Self {
        let peer = extensions
            .get::<ConnectInfo<SocketAddr>>()
            .map(|c| canonical(c.0.ip()));
        let Some(peer) = peer else { return Self(None) };
        if !state.trusted_proxies.contains(peer) {
            return Self(Some(peer));
        }
        let mut chain: Vec<IpAddr> = headers
            .get_all("x-forwarded-for")
            .iter()
            .filter_map(|v| v.to_str().ok())
            .flat_map(|v| v.split(','))
            .filter_map(|s| s.trim().parse().ok())
            .map(canonical)
            .collect();
        while let Some(ip) = chain.pop() {
            if !state.trusted_proxies.contains(ip) {
                return Self(Some(ip));
            }
        }
        Self(Some(peer))
    }
}

impl FromRequestParts<AppState> for ClientIp {
    type Rejection = std::convert::Infallible;
    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        Ok(Self::from_parts(state, &parts.extensions, &parts.headers))
    }
}

/// Session facts for a new sign-in.
#[must_use]
pub fn session_info(ip: ClientIp, headers: &HeaderMap) -> NewSessionInfo {
    NewSessionInfo {
        ip: ip.0.map(|i| i.to_string()),
        user_agent: headers
            .get(header::USER_AGENT)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned),
    }
}

/// The CSRF seed of this browser: its session cookie, or a pre-session
/// random value. `fresh` seeds still have to be sent as a cookie.
#[derive(Debug, Clone)]
pub struct CsrfSeed {
    pub value: String,
    pub fresh: bool,
}

/// The authenticated browser session, if any.
#[derive(Debug, Clone)]
pub struct MaybeSession(pub Option<Session>);

impl MaybeSession {
    #[must_use]
    pub fn subject(&self) -> Subject {
        self.0
            .as_ref()
            .map_or(Subject::Anonymous, |s| s.subject.clone())
    }
}

impl<S: Send + Sync> FromRequestParts<S> for MaybeSession {
    type Rejection = std::convert::Infallible;
    async fn from_request_parts(parts: &mut Parts, _: &S) -> Result<Self, Self::Rejection> {
        Ok(parts
            .extensions
            .get::<Self>()
            .cloned()
            .unwrap_or(Self(None)))
    }
}

/// A signed-in browser session; rejects with 401 (sign-in page) otherwise.
#[derive(Debug, Clone)]
pub struct RequireSession(pub Session);

impl<S: Send + Sync> FromRequestParts<S> for RequireSession {
    type Rejection = AppError;
    async fn from_request_parts(parts: &mut Parts, _: &S) -> Result<Self, Self::Rejection> {
        match parts.extensions.get::<MaybeSession>() {
            Some(MaybeSession(Some(s))) => Ok(Self(s.clone())),
            _ => Err(AppError::unauthenticated()),
        }
    }
}

/// Resolves the session cookie and the CSRF seed for UI requests. A session
/// cookie that no longer resolves (revoked, expired) is cleared.
pub async fn resolve(State(state): State<AppState>, mut req: Request, next: Next) -> Response {
    let name = session_cookie_name(&state);
    let raw = cookie(req.headers(), name).map(str::to_owned);
    let session = match &raw {
        Some(c) => match state.services.resolve_session(c).await {
            Ok(s) => s,
            Err(e) => return AppError::internal(&e).into_response(),
        },
        None => None,
    };
    let stale = raw.is_some() && session.is_none();
    let seed = match (&session, &raw) {
        (Some(_), Some(c)) => CsrfSeed {
            value: c.clone(),
            fresh: false,
        },
        _ => match cookie(req.headers(), seed_cookie_name(&state)) {
            Some(v) if v.len() == 43 => CsrfSeed {
                value: v.to_owned(),
                fresh: false,
            },
            _ => match slath_auth::token::new_session_id() {
                Ok(v) => CsrfSeed {
                    value: v.expose().clone(),
                    fresh: true,
                },
                Err(e) => return AppError::internal(&e).into_response(),
            },
        },
    };
    let maybe = MaybeSession(session);
    req.extensions_mut().insert(maybe.clone());
    req.extensions_mut().insert(seed.clone());
    let mut res = next.run(req).await;
    // Error pages are rendered outside this middleware; let them know who's signed in.
    res.extensions_mut().insert(maybe);
    if stale
        && !res
            .headers()
            .get_all(header::SET_COOKIE)
            .iter()
            .any(|v| v.as_bytes().starts_with(name.as_bytes()))
    {
        res.headers_mut()
            .append(header::SET_COOKIE, set_cookie(&state, name, "", None));
    }
    // Pages may contain forms; give the browser the seed their tokens use.
    let is_html = res
        .headers()
        .get(header::CONTENT_TYPE)
        .is_some_and(|v| v.as_bytes().starts_with(b"text/html"));
    if seed.fresh && is_html {
        res.headers_mut().append(
            header::SET_COOKIE,
            set_cookie(
                &state,
                seed_cookie_name(&state),
                &seed.value,
                Some(7 * 86_400),
            ),
        );
    }
    res
}

/// Upper bound for bodies the CSRF check buffers to find the form token.
const MAX_FORM: usize = 1024 * 1024;

fn same_origin(state: &AppState, headers: &HeaderMap) -> bool {
    match headers.get("sec-fetch-site").and_then(|v| v.to_str().ok()) {
        Some(site) => matches!(site, "same-origin" | "none"),
        None => headers
            .get(header::ORIGIN)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|o| o == state.config.server.public_url.origin()),
    }
}

/// Rejects cross-site state changes: non-GET UI requests need a same-origin
/// `Sec-Fetch-Site` (or `Origin`) **and** a valid token in `X-CSRF-Token` or
/// the `_csrf` form field.
pub async fn csrf(State(state): State<AppState>, req: Request, next: Next) -> Response {
    if matches!(*req.method(), Method::GET | Method::HEAD | Method::OPTIONS) {
        return next.run(req).await;
    }
    let forbidden = || {
        AppError::forbidden()
            .detail(
                "The request didn't come from this site's forms. Reload the page and try again.",
            )
            .into_response()
    };
    if !same_origin(&state, req.headers()) {
        return forbidden();
    }
    let Some(seed) = req.extensions().get::<CsrfSeed>().cloned() else {
        return forbidden();
    };
    if seed.fresh {
        // A browser without seed cookie never received a form from us.
        return forbidden();
    }
    if let Some(t) = req
        .headers()
        .get("x-csrf-token")
        .and_then(|v| v.to_str().ok())
    {
        return if state.services.csrf.verify(&seed.value, t) {
            next.run(req).await
        } else {
            forbidden()
        };
    }
    let is_form = req
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|ct| ct.starts_with("application/x-www-form-urlencoded"));
    if !is_form {
        return forbidden();
    }
    let (parts, body) = req.into_parts();
    let Ok(bytes) = axum::body::to_bytes(body, MAX_FORM).await else {
        return AppError::from_status(axum::http::StatusCode::PAYLOAD_TOO_LARGE).into_response();
    };
    let token = form_field(&bytes, "_csrf");
    if !token.is_some_and(|t| state.services.csrf.verify(&seed.value, &t)) {
        return forbidden();
    }
    next.run(Request::from_parts(parts, Body::from(bytes)))
        .await
}

/// The first value of `name` in a URL-encoded form body.
#[must_use]
pub fn form_field(body: &[u8], name: &str) -> Option<String> {
    body.split(|&b| b == b'&').find_map(|pair| {
        let (k, v) = match pair.iter().position(|&b| b == b'=') {
            Some(i) => (&pair[..i], &pair[i + 1..]),
            None => (pair, &b""[..]),
        };
        (url_decode(k)? == name).then(|| url_decode(v)).flatten()
    })
}

fn url_decode(s: &[u8]) -> Option<String> {
    let plus: Vec<u8> = s
        .iter()
        .map(|&b| if b == b'+' { b' ' } else { b })
        .collect();
    percent_encoding::percent_decode(&plus)
        .decode_utf8()
        .ok()
        .map(|c| c.into_owned())
}

/// A redirect target from user input (`?next=`): only a local absolute path.
/// Anything else (other origins, `//host`, backslashes, control characters,
/// schemes) falls back to `/`.
#[must_use]
pub fn safe_next(next: Option<&str>) -> String {
    let Some(n) = next else { return "/".to_owned() };
    let ok = n.starts_with('/')
        && !n.starts_with("//")
        && !n.starts_with("/\\")
        && n.len() <= 2048
        && !n
            .bytes()
            .any(|b| b.is_ascii_control() || b == b'\\' || b == b' ')
        && !n.to_ascii_lowercase().contains("%2f%2f")
        && !n.to_ascii_lowercase().starts_with("/%2f")
        && !n.to_ascii_lowercase().starts_with("/%5c");
    if ok { n.to_owned() } else { "/".to_owned() }
}

/// Base64url without padding (for small random values in URLs).
#[must_use]
pub fn b64url(bytes: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safe_next_rejects_open_redirects() {
        for ok in ["/", "/alice/repo", "/-/settings/keys?x=1#frag"] {
            assert_eq!(safe_next(Some(ok)), ok);
        }
        for bad in [
            "//evil.com",
            "/\\evil.com",
            "/%2F%2Fevil.com",
            "/%2f%2fevil.com",
            "/%5Cevil.com",
            "https://evil.com",
            "https:evil.com",
            "/\t/evil.com",
            "\\\\evil.com",
            "javascript:alert(1)",
            "/%0d%0aSet-Cookie:x=y".replace("%0d%0a", "\r\n").as_str(),
            "evil.com",
            "",
        ] {
            assert_eq!(safe_next(Some(bad)), "/", "{bad:?}");
        }
        assert_eq!(safe_next(None), "/");
    }

    #[test]
    fn trusted_proxy_networks() {
        let t = TrustedProxies::parse(&[
            "127.0.0.1/32".into(),
            "10.0.0.0/8".into(),
            "fd00::/8".into(),
            "192.168.1.5".into(),
        ])
        .unwrap();
        assert!(t.contains("127.0.0.1".parse().unwrap()));
        assert!(t.contains("::ffff:127.0.0.1".parse().unwrap()));
        assert!(t.contains("10.20.30.40".parse().unwrap()));
        assert!(!t.contains("11.0.0.1".parse().unwrap()));
        assert!(t.contains("fd12::1".parse().unwrap()));
        assert!(t.contains("192.168.1.5".parse().unwrap()));
        assert!(!t.contains("192.168.1.6".parse().unwrap()));
        assert!(TrustedProxies::parse(&["10.0.0.0/33".into()]).is_err());
        assert!(TrustedProxies::parse(&["nope".into()]).is_err());
    }

    #[test]
    fn form_fields() {
        assert_eq!(
            form_field(b"a=1&_csrf=x%2By+z&b", "_csrf").as_deref(),
            Some("x+y z")
        );
        assert_eq!(form_field(b"a=1", "_csrf"), None);
    }
}

/// A signed-in session in sudo mode. Without sudo mode, the request is
/// redirected to the confirmation page, which returns to the page the user
/// was on (for GET) or came from (for form posts).
#[derive(Debug, Clone)]
pub struct Sudo(pub Session);

impl FromRequestParts<AppState> for Sudo {
    type Rejection = Response;
    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let session = match parts.extensions.get::<MaybeSession>() {
            Some(MaybeSession(Some(s))) => s.clone(),
            _ => return Err(AppError::unauthenticated().into_response()),
        };
        if session.in_sudo() {
            return Ok(Self(session));
        }
        let back = if parts.method == Method::GET {
            parts
                .uri
                .path_and_query()
                .map_or("/", |p| p.as_str())
                .to_owned()
        } else {
            // Return to the page with the form, if it was ours.
            let origin = state.config.server.public_url.origin();
            parts
                .headers
                .get(header::REFERER)
                .and_then(|v| v.to_str().ok())
                .and_then(|r| r.strip_prefix(origin))
                .map_or_else(|| "/-/settings".to_owned(), str::to_owned)
        };
        let next = safe_next(Some(&back));
        let target = format!(
            "/-/sudo?next={}",
            percent_encoding::utf8_percent_encode(&next, percent_encoding::NON_ALPHANUMERIC)
        );
        Err(axum::response::Redirect::to(&target).into_response())
    }
}
