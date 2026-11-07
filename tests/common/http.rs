// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! A small HTTP client for tests: no redirects followed, error statuses
//! returned as responses, and a cookie jar that tests can inspect.

use std::collections::BTreeMap;

/// A response with everything tests look at.
#[derive(Debug, Clone)]
pub struct Resp {
    pub status: u16,
    /// Header names in lowercase; repeated headers joined with `\n`.
    pub headers: BTreeMap<String, String>,
    pub body: String,
    pub bytes: Vec<u8>,
}

impl Resp {
    #[must_use]
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .get(&name.to_ascii_lowercase())
            .map(String::as_str)
    }

    /// All `Set-Cookie` lines.
    #[must_use]
    pub fn set_cookies(&self) -> Vec<&str> {
        self.header("set-cookie")
            .map_or_else(Vec::new, |v| v.split('\n').collect())
    }

    #[must_use]
    pub fn json(&self) -> serde_json::Value {
        serde_json::from_slice(&self.bytes)
            .unwrap_or_else(|e| panic!("not JSON ({e}): {}", self.body))
    }
}

/// A client bound to one server origin. Cookies from responses are kept and
/// sent with later requests.
pub struct Client {
    agent: ureq::Agent,
    pub base: String,
    pub cookies: BTreeMap<String, String>,
    /// Extra headers for every request (e.g. `Origin`, `Sec-Fetch-Site`).
    pub default_headers: Vec<(String, String)>,
}

impl Client {
    #[must_use]
    pub fn new(base: &str) -> Self {
        let agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .max_redirects(0)
            .build()
            .new_agent();
        Self {
            agent,
            base: base.trim_end_matches('/').to_owned(),
            cookies: BTreeMap::new(),
            default_headers: vec![
                ("sec-fetch-site".to_owned(), "same-origin".to_owned()),
                ("origin".to_owned(), base.trim_end_matches('/').to_owned()),
            ],
        }
    }

    fn url(&self, path: &str) -> String {
        if path.starts_with("http") {
            path.to_owned()
        } else {
            format!("{}{path}", self.base)
        }
    }

    fn cookie_header(&self) -> Option<String> {
        (!self.cookies.is_empty()).then(|| {
            self.cookies
                .iter()
                .map(|(k, v)| format!("{k}={v}"))
                .collect::<Vec<_>>()
                .join("; ")
        })
    }

    fn finish(&mut self, res: Result<ureq::http::Response<ureq::Body>, ureq::Error>) -> Resp {
        let mut res = res.unwrap();
        let status = res.status().as_u16();
        let mut headers: BTreeMap<String, String> = BTreeMap::new();
        for (k, v) in res.headers() {
            let v = String::from_utf8_lossy(v.as_bytes()).into_owned();
            headers
                .entry(k.as_str().to_owned())
                .and_modify(|e| {
                    e.push('\n');
                    e.push_str(&v);
                })
                .or_insert(v);
        }
        let bytes = res
            .body_mut()
            .with_config()
            .limit(64 * 1024 * 1024)
            .read_to_vec()
            .unwrap_or_default();
        if let Some(sc) = headers.get("set-cookie") {
            for line in sc.split('\n') {
                let first = line.split(';').next().unwrap_or("");
                if let Some((k, v)) = first.split_once('=') {
                    let expired = line.to_ascii_lowercase().contains("max-age=0");
                    if v.is_empty() || expired {
                        self.cookies.remove(k.trim());
                    } else {
                        self.cookies.insert(k.trim().to_owned(), v.to_owned());
                    }
                }
            }
        }
        Resp {
            status,
            headers,
            body: String::from_utf8_lossy(&bytes).into_owned(),
            bytes,
        }
    }

    /// Sends a request with optional body and extra headers.
    pub fn request(
        &mut self,
        method: &str,
        path: &str,
        headers: &[(&str, &str)],
        body: Option<(&str, Vec<u8>)>,
    ) -> Resp {
        let mut b = ureq::http::Request::builder()
            .method(method)
            .uri(self.url(path));
        for (k, v) in &self.default_headers {
            if !headers.iter().any(|(h, _)| h.eq_ignore_ascii_case(k)) {
                b = b.header(k.as_str(), v.as_str());
            }
        }
        if let Some(c) = self.cookie_header()
            && !headers
                .iter()
                .any(|(h, _)| h.eq_ignore_ascii_case("cookie"))
        {
            b = b.header("cookie", c);
        }
        for (k, v) in headers {
            b = b.header(*k, *v);
        }
        let (ct, data) = body.unwrap_or(("", Vec::new()));
        if !ct.is_empty() {
            b = b.header("content-type", ct);
        }
        let parts = b.body(()).unwrap().into_parts().0;
        let mut res = self
            .agent
            .run(ureq::http::Request::from_parts(parts.clone(), data.clone()));
        // A pooled keep-alive connection the server already closed (e.g.
        // after answering early without reading a body) fails at once;
        // like browsers, retry such a request once on a fresh connection.
        if let Err(ureq::Error::Io(e)) = &res
            && matches!(
                e.kind(),
                std::io::ErrorKind::UnexpectedEof
                    | std::io::ErrorKind::ConnectionReset
                    | std::io::ErrorKind::BrokenPipe
            )
        {
            res = self.agent.run(ureq::http::Request::from_parts(parts, data));
        }
        self.finish(res)
    }

    pub fn get(&mut self, path: &str) -> Resp {
        self.request("GET", path, &[], None)
    }

    pub fn get_with(&mut self, path: &str, headers: &[(&str, &str)]) -> Resp {
        self.request("GET", path, headers, None)
    }

    /// POSTs a URL-encoded form.
    pub fn post_form(&mut self, path: &str, fields: &[(&str, &str)]) -> Resp {
        self.post_form_with(path, fields, &[])
    }

    pub fn post_form_with(
        &mut self,
        path: &str,
        fields: &[(&str, &str)],
        headers: &[(&str, &str)],
    ) -> Resp {
        let body = form_encode(fields);
        self.request(
            "POST",
            path,
            headers,
            Some(("application/x-www-form-urlencoded", body.into_bytes())),
        )
    }

    /// Sends JSON with a bearer token (API requests send no cookies).
    pub fn api(
        &mut self,
        method: &str,
        path: &str,
        token: Option<&str>,
        json: Option<&serde_json::Value>,
    ) -> Resp {
        self.api_with(method, path, token, json, &[])
    }

    pub fn api_with(
        &mut self,
        method: &str,
        path: &str,
        token: Option<&str>,
        json: Option<&serde_json::Value>,
        extra: &[(&str, &str)],
    ) -> Resp {
        let auth = token.map(|t| format!("Bearer {t}"));
        let mut headers: Vec<(&str, &str)> = vec![("cookie", "")];
        if let Some(a) = &auth {
            headers.push(("authorization", a));
        }
        headers.extend_from_slice(extra);
        let body = json.map(|j| ("application/json", serde_json::to_vec(j).unwrap()));
        self.request(method, path, &headers, body)
    }
}

/// `application/x-www-form-urlencoded` encoding.
#[must_use]
pub fn form_encode(fields: &[(&str, &str)]) -> String {
    fields
        .iter()
        .map(|(k, v)| format!("{}={}", enc(k), enc(v)))
        .collect::<Vec<_>>()
        .join("&")
}

fn enc(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            b' ' => out.push('+'),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// The `_csrf` hidden field of the first form in a page.
#[must_use]
pub fn csrf_token(html: &str) -> Option<String> {
    let i = html.find("name=\"_csrf\" value=\"")?;
    let rest = &html[i + "name=\"_csrf\" value=\"".len()..];
    rest.split('"').next().map(str::to_owned)
}

/// Loads a page and submits a form to `action` with its CSRF token.
pub fn submit(c: &mut Client, page: &str, action: &str, fields: &[(&str, &str)]) -> Resp {
    let p = c.get(page);
    let token = csrf_token(&p.body)
        .unwrap_or_else(|| panic!("no CSRF token on {page} ({}): {}", p.status, p.body));
    let mut all = vec![("_csrf", token.as_str())];
    all.extend_from_slice(fields);
    c.post_form(action, &all)
}

/// Signs in through the login form.
pub fn login(c: &mut Client, name: &str, password: &str) -> Resp {
    submit(
        c,
        "/-/login",
        "/-/login",
        &[("username", name), ("password", password)],
    )
}

/// Creates a repository through the UI (expects success).
pub fn create_repo(c: &mut Client, ns: &str, name: &str, visibility: &str) -> Resp {
    let r = submit(
        c,
        "/-/new",
        "/-/new",
        &[
            ("namespace", ns),
            ("name", name),
            ("description", ""),
            ("visibility", visibility),
            ("default_branch", "main"),
        ],
    );
    assert_eq!(r.status, 303, "create {ns}/{name}: {}", r.body);
    r
}

/// Creates a token through the settings page and returns it.
pub fn create_token(c: &mut Client, name: &str, scopes: &[&str]) -> String {
    let mut fields = vec![("name", name), ("expires", "30")];
    for s in scopes {
        fields.push(("scopes", s));
    }
    let r = submit(c, "/-/settings/tokens", "/-/settings/tokens", &fields);
    assert_eq!(r.status, 200, "{}", r.body);
    let start = r.body.find("value=\"slath_pat_").expect("token shown") + "value=\"".len();
    r.body[start..start + slath_core::credential::TOKEN_LEN].to_owned()
}

/// One part of a multipart form: name, file name (for file parts), content.
pub type Part<'a> = (&'a str, Option<&'a str>, &'a [u8]);

/// A `multipart/form-data` body and its content type.
#[must_use]
pub fn multipart(parts: &[Part<'_>]) -> (String, Vec<u8>) {
    let boundary = "slath-test-boundary-7f3a";
    let mut body = Vec::new();
    for (name, file, data) in parts {
        body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
        match file {
            Some(f) => body.extend_from_slice(
                format!(
                    "Content-Disposition: form-data; name=\"{name}\"; filename=\"{f}\"\r\nContent-Type: application/octet-stream\r\n\r\n"
                )
                .as_bytes(),
            ),
            None => body.extend_from_slice(
                format!("Content-Disposition: form-data; name=\"{name}\"\r\n\r\n").as_bytes(),
            ),
        }
        body.extend_from_slice(data);
        body.extend_from_slice(b"\r\n");
    }
    body.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
    (format!("multipart/form-data; boundary={boundary}"), body)
}
