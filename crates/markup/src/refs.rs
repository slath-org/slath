// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! References in Markdown: `#12`, `ns/repo#12`, `@user`, and commit IDs.
//!
//! Rendering is cached without knowing the viewer, so the renderer only
//! marks references: each becomes a link to `#slath-ref:<kind>:<target>`
//! with a canonical target. [`resolve`] then replaces every such link per
//! viewer, either with a real link (title and state included) or with the
//! reference as plain text, which reveals neither existence nor title. The
//! visible text always comes from the canonical target, so a hand-written
//! `[click here](#slath-ref:…)` can't disguise a reference.

use std::sync::{Arc, Mutex};

use comrak::nodes::{AstNode, NodeLink, NodeValue};

const MARK: &str = "#slath-ref:";
/// Longest issue number accepted in a reference.
const MAX_NUMBER: u64 = 999_999_999;
/// References resolved per document; more stay plain text.
pub const MAX_REFS: usize = 200;

/// A reference found in Markdown.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Ref {
    /// An issue or merge request (they share numbers).
    Item {
        ns: String,
        repo: String,
        number: u64,
    },
    /// A commit by (abbreviated) ID.
    Commit {
        ns: String,
        repo: String,
        sha: String,
    },
    /// A user.
    Mention { name: String },
}

impl Ref {
    fn token(&self) -> String {
        match self {
            Self::Item { ns, repo, number } => format!("item:{ns}/{repo}:{number}"),
            Self::Commit { ns, repo, sha } => format!("commit:{ns}/{repo}:{sha}"),
            Self::Mention { name } => format!("user:{name}"),
        }
    }

    fn parse_token(t: &str) -> Option<Self> {
        let (kind, rest) = t.split_once(':')?;
        match kind {
            "user" => valid_name(rest).then(|| Self::Mention {
                name: rest.to_owned(),
            }),
            "item" | "commit" => {
                let (path, id) = rest.rsplit_once(':')?;
                let (ns, repo) = path.split_once('/')?;
                if !valid_name(ns) || !valid_name(repo) {
                    return None;
                }
                if kind == "item" {
                    let number: u64 = id.parse().ok().filter(|n| (1..=MAX_NUMBER).contains(n))?;
                    if id.starts_with('0') {
                        return None;
                    }
                    Some(Self::Item {
                        ns: ns.to_owned(),
                        repo: repo.to_owned(),
                        number,
                    })
                } else {
                    is_sha(id).then(|| Self::Commit {
                        ns: ns.to_owned(),
                        repo: repo.to_owned(),
                        sha: id.to_owned(),
                    })
                }
            }
            _ => None,
        }
    }

    /// The text shown for the reference, relative to the document's repository.
    #[must_use]
    pub fn text(&self, ctx: Option<&RefContext>) -> String {
        let local = |ns: &str, repo: &str| {
            ctx.is_some_and(|c| c.ns.eq_ignore_ascii_case(ns) && c.repo.eq_ignore_ascii_case(repo))
        };
        match self {
            Self::Item { ns, repo, number } if local(ns, repo) => format!("#{number}"),
            Self::Item { ns, repo, number } => format!("{ns}/{repo}#{number}"),
            Self::Commit { ns, repo, sha } if local(ns, repo) => {
                sha[..sha.len().min(12)].to_owned()
            }
            Self::Commit { ns, repo, sha } => format!("{ns}/{repo}@{}", &sha[..sha.len().min(12)]),
            Self::Mention { name } => format!("@{name}"),
        }
    }
}

/// The repository a document belongs to (for `#12` and bare commit IDs).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RefContext {
    pub ns: String,
    pub repo: String,
}

fn is_name_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-')
}

fn valid_name(s: &str) -> bool {
    let b = s.as_bytes();
    !b.is_empty()
        && b.len() <= 39
        && b[0].is_ascii_alphanumeric()
        && b.iter().all(|c| is_name_byte(*c))
        && !s.ends_with('.')
}

fn is_sha(s: &str) -> bool {
    (7..=40).contains(&s.len())
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        && s.bytes().any(|b| (b'a'..=b'f').contains(&b))
}

/// A reference found in a text, as a byte range.
struct Found {
    start: usize,
    end: usize,
    r: Ref,
}

/// Scans plain text for references.
fn scan(text: &str, ctx: Option<&RefContext>) -> Vec<Found> {
    let b = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    // A reference starts at the beginning or after a byte that can't be
    // part of a name (so `a#1`, `x@y.org`, and `ab/cd#` inside words don't count).
    let boundary = |i: usize| {
        i == 0 || !(is_name_byte(b[i - 1]) || matches!(b[i - 1], b'/' | b'@' | b'#' | b'&'))
    };
    let end_ok = |j: usize| j == b.len() || !(b[j].is_ascii_alphanumeric() || b[j] == b'_');
    while i < b.len() {
        if !boundary(i) {
            i += 1;
            continue;
        }
        // #123
        if b[i] == b'#' {
            let j = i + 1 + b[i + 1..].iter().take_while(|c| c.is_ascii_digit()).count();
            if j > i + 1 && b[i + 1] != b'0' && end_ok(j) {
                if let (Some(c), Ok(n)) = (ctx, text[i + 1..j].parse::<u64>())
                    && n <= MAX_NUMBER
                {
                    out.push(Found {
                        start: i,
                        end: j,
                        r: Ref::Item {
                            ns: c.ns.clone(),
                            repo: c.repo.clone(),
                            number: n,
                        },
                    });
                }
                i = j;
                continue;
            }
        }
        // @user
        if b[i] == b'@' {
            let j = i + 1 + b[i + 1..].iter().take_while(|c| is_name_byte(**c)).count();
            let mut name = &text[i + 1..j];
            name = name.trim_end_matches('.');
            let j = i + 1 + name.len();
            if valid_name(name) && end_ok(j) && (j == b.len() || b[j] != b'/') {
                out.push(Found {
                    start: i,
                    end: j,
                    r: Ref::Mention {
                        name: name.to_owned(),
                    },
                });
                i = j;
                continue;
            }
        }
        // ns/repo#123 and ns/repo@sha
        if b[i].is_ascii_alphanumeric() {
            let ns_end = i + b[i..].iter().take_while(|c| is_name_byte(**c)).count();
            if ns_end < b.len() && b[ns_end] == b'/' {
                let repo_end = ns_end
                    + 1
                    + b[ns_end + 1..]
                        .iter()
                        .take_while(|c| is_name_byte(**c))
                        .count();
                let ns = &text[i..ns_end];
                let repo = &text[ns_end + 1..repo_end];
                if repo_end < b.len() && valid_name(ns) && valid_name(repo) {
                    let sep = b[repo_end];
                    let k = repo_end + 1;
                    if sep == b'#' {
                        let j = k + b[k..].iter().take_while(|c| c.is_ascii_digit()).count();
                        if j > k
                            && b[k] != b'0'
                            && end_ok(j)
                            && let Ok(n) = text[k..j].parse::<u64>()
                            && n <= MAX_NUMBER
                        {
                            out.push(Found {
                                start: i,
                                end: j,
                                r: Ref::Item {
                                    ns: ns.to_owned(),
                                    repo: repo.to_owned(),
                                    number: n,
                                },
                            });
                            i = j;
                            continue;
                        }
                    } else if sep == b'@' {
                        let j = k + b[k..].iter().take_while(|c| c.is_ascii_hexdigit()).count();
                        let sha = text[k..j].to_ascii_lowercase();
                        if is_sha(&sha) && end_ok(j) {
                            out.push(Found {
                                start: i,
                                end: j,
                                r: Ref::Commit {
                                    ns: ns.to_owned(),
                                    repo: repo.to_owned(),
                                    sha,
                                },
                            });
                            i = j;
                            continue;
                        }
                    }
                }
            }
            // A bare commit ID in the document's repository.
            let j = i + b[i..].iter().take_while(|c| c.is_ascii_hexdigit()).count();
            if let Some(c) = ctx {
                let word = &text[i..j];
                if j - i >= 7
                    && end_ok(j)
                    && word.bytes().all(|x| !x.is_ascii_uppercase())
                    && is_sha(word)
                {
                    out.push(Found {
                        start: i,
                        end: j,
                        r: Ref::Commit {
                            ns: c.ns.clone(),
                            repo: c.repo.clone(),
                            sha: word.to_owned(),
                        },
                    });
                    i = j;
                    continue;
                }
            }
            i = ns_end.max(i + 1);
            continue;
        }
        i += 1;
    }
    out
}

fn inside_code_or_link<'a>(node: &'a AstNode<'a>) -> bool {
    node.ancestors().skip(1).any(|a| {
        matches!(
            a.data.borrow().value,
            NodeValue::Link(_) | NodeValue::Image(_) | NodeValue::Code(_) | NodeValue::CodeBlock(_)
        )
    })
}

/// Replaces references in the text nodes under `root` with marker links.
pub(crate) fn mark<'a>(
    arena: &'a comrak::Arena<'a>,
    root: &'a AstNode<'a>,
    ctx: Option<&RefContext>,
) {
    let texts: Vec<&AstNode<'_>> = root
        .descendants()
        .filter(|n| matches!(n.data.borrow().value, NodeValue::Text(_)))
        .collect();
    let mut total = 0;
    for node in texts {
        if total >= MAX_REFS || inside_code_or_link(node) {
            continue;
        }
        let text = match &node.data.borrow().value {
            NodeValue::Text(t) => t.to_string(),
            _ => continue,
        };
        let found = scan(&text, ctx);
        if found.is_empty() {
            continue;
        }
        let mut pos = 0;
        for f in found.into_iter().take(MAX_REFS - total) {
            total += 1;
            if f.start > pos {
                node.insert_before(
                    arena.alloc(NodeValue::Text(text[pos..f.start].to_owned().into()).into()),
                );
            }
            let link = arena.alloc(
                NodeValue::Link(Box::new(NodeLink {
                    url: format!("{MARK}{}", f.r.token()),
                    title: String::new(),
                }))
                .into(),
            );
            link.append(
                arena.alloc(NodeValue::Text(text[f.start..f.end].to_owned().into()).into()),
            );
            node.insert_before(link);
            pos = f.end;
        }
        if pos < text.len() {
            node.insert_before(arena.alloc(NodeValue::Text(text[pos..].to_owned().into()).into()));
        }
        node.detach();
    }
}

/// What a reference becomes for one viewer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolved {
    /// A link; `title` is shown on hover, `state` becomes a CSS class
    /// (`open`, `closed`, `merged`).
    Link {
        href: String,
        title: Option<String>,
        state: Option<&'static str>,
    },
    /// Plain text (unknown, invisible, or not a reference after all).
    Text,
}

/// The marker links in rendered HTML, in order: `(start, end, reference)`.
fn markers(html: &str) -> Vec<(usize, usize, Option<Ref>)> {
    let mut out = Vec::new();
    let mut from = 0;
    while let Some(off) = html[from..].find("<a href=\"#slath-ref:") {
        let start = from + off;
        let href_start = start + "<a href=\"".len();
        let Some(href_len) = html[href_start..].find('"') else {
            break;
        };
        let token = &html[href_start + MARK.len()..href_start + href_len];
        let Some(close) = html[start..].find("</a>") else {
            break;
        };
        let end = start + close + "</a>".len();
        out.push((start, end, Ref::parse_token(&decode_attr(token))));
        from = end;
    }
    out
}

fn decode_attr(s: &str) -> String {
    s.replace("&amp;", "&")
}

/// The references in rendered HTML (deduplicated, in order).
#[must_use]
pub fn references(html: &str) -> Vec<Ref> {
    let mut out: Vec<Ref> = Vec::new();
    for (_, _, r) in markers(html) {
        if let Some(r) = r
            && !out.contains(&r)
        {
            out.push(r);
        }
    }
    out
}

fn escape(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => o.push_str("&amp;"),
            '<' => o.push_str("&lt;"),
            '>' => o.push_str("&gt;"),
            '"' => o.push_str("&quot;"),
            '\'' => o.push_str("&#39;"),
            c => o.push(c),
        }
    }
    o
}

/// Replaces every marker with what `f` decides for the viewer. The text of
/// a reference always comes from its canonical form.
pub fn resolve(
    html: &str,
    ctx: Option<&RefContext>,
    mut f: impl FnMut(&Ref) -> Resolved,
) -> String {
    let mut out = String::with_capacity(html.len());
    let mut pos = 0;
    for (start, end, r) in markers(html) {
        out.push_str(&html[pos..start]);
        // A malformed marker (hand-written) is dropped.
        if let Some(r) = r {
            let text = escape(&r.text(ctx));
            match f(&r) {
                Resolved::Link { href, title, state } => {
                    out.push_str("<a class=\"ref");
                    if let Some(s) = state {
                        out.push_str(" ref-");
                        out.push_str(s);
                    }
                    out.push_str("\" href=\"");
                    out.push_str(&escape(&href));
                    out.push('"');
                    if let Some(t) = title {
                        out.push_str(" title=\"");
                        out.push_str(&escape(&t));
                        out.push('"');
                    }
                    out.push('>');
                    out.push_str(&text);
                    out.push_str("</a>");
                }
                Resolved::Text => out.push_str(&text),
            }
        }
        pos = end;
    }
    out.push_str(&html[pos..]);
    out
}

/// Cache of viewer-independent renders, keyed by source, context, and options.
type Entries = (
    std::collections::HashMap<[u8; 32], Arc<str>>,
    std::collections::VecDeque<[u8; 32]>,
);

pub(crate) struct Cache {
    entries: Mutex<Entries>,
    capacity: usize,
}

impl Cache {
    pub(crate) fn new(capacity: usize) -> Self {
        Self {
            entries: Mutex::default(),
            capacity,
        }
    }

    pub(crate) fn get_or(&self, key: [u8; 32], render: impl FnOnce() -> String) -> Arc<str> {
        if let Ok(e) = self.entries.lock()
            && let Some(v) = e.0.get(&key)
        {
            return Arc::clone(v);
        }
        let v: Arc<str> = render().into();
        if let Ok(mut e) = self.entries.lock() {
            if e.0.len() >= self.capacity
                && let Some(old) = e.1.pop_front()
            {
                e.0.remove(&old);
            }
            if e.0.insert(key, Arc::clone(&v)).is_none() {
                e.1.push_back(key);
            }
        }
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> RefContext {
        RefContext {
            ns: "alice".into(),
            repo: "demo".into(),
        }
    }

    fn found(text: &str) -> Vec<String> {
        scan(text, Some(&ctx()))
            .into_iter()
            .map(|f| f.r.token())
            .collect()
    }

    #[test]
    fn scanning() {
        assert_eq!(
            found("see #12 and acme/tools#3."),
            ["item:alice/demo:12", "item:acme/tools:3"]
        );
        assert_eq!(found("ping @bob, @carol."), ["user:bob", "user:carol"]);
        assert_eq!(
            found("fixed in deadbeef1 and acme/x@CAFEBABE12"),
            ["commit:alice/demo:deadbeef1", "commit:acme/x:cafebabe12"]
        );
        for none in [
            "a#12",
            "#0",
            "#012",
            "x@y.org",
            "1234567",
            "deadbe",
            "abc#12x",
            "&#12;",
            "@org/team",
            "path/x#12abc",
        ] {
            assert!(found(none).is_empty(), "{none}: {:?}", found(none));
        }
        // Without a repository context, short forms aren't references.
        assert!(scan("#12 deadbeef1", None).is_empty());
    }

    #[test]
    fn tokens_round_trip_and_reject_junk() {
        for r in [
            Ref::Item {
                ns: "a".into(),
                repo: "b".into(),
                number: 7,
            },
            Ref::Commit {
                ns: "a".into(),
                repo: "b".into(),
                sha: "deadbeef".into(),
            },
            Ref::Mention { name: "bob".into() },
        ] {
            assert_eq!(Ref::parse_token(&r.token()), Some(r));
        }
        for bad in [
            "item:a/b:0",
            "item:a/b:-1",
            "item:../b:1",
            "item:a/b:01",
            "commit:a/b:zz",
            "user:",
            "user:a/b",
            "x:y",
        ] {
            assert_eq!(Ref::parse_token(bad), None, "{bad}");
        }
    }

    #[test]
    fn rendering_marks_text_only() {
        let c = ctx();
        let html = crate::render_with_refs(
            "Fixes #1, see `#2`, [#3](https://x.org), ~~#4~~ and\n\n```\n#5\n```\n\n[spoof](#slath-ref:item:alice/demo:9)",
            crate::MarkdownOptions::default(),
            Some(&c),
        );
        let refs = references(&html);
        assert_eq!(
            refs,
            vec![
                Ref::Item {
                    ns: "alice".into(),
                    repo: "demo".into(),
                    number: 1
                },
                Ref::Item {
                    ns: "alice".into(),
                    repo: "demo".into(),
                    number: 4
                },
                Ref::Item {
                    ns: "alice".into(),
                    repo: "demo".into(),
                    number: 9
                },
            ]
        );
        let out = resolve(&html, Some(&c), |_| Resolved::Text);
        assert!(out.contains("<code>#2</code>") && out.contains(">#3</a>") && out.contains("#5"));
        assert!(
            !out.contains("spoof") && !out.contains("slath-ref"),
            "{out}"
        );
    }

    #[test]
    fn resolving_uses_canonical_text() {
        let html =
            "<p>x <a href=\"#slath-ref:item:alice/demo:3\" rel=\"nofollow\">click me</a> y</p>";
        let out = resolve(html, Some(&ctx()), |_| Resolved::Text);
        assert_eq!(out, "<p>x #3 y</p>");
        let out = resolve(html, Some(&ctx()), |_| Resolved::Link {
            href: "/alice/demo/issues/3".into(),
            title: Some("A <b>title</b>".into()),
            state: Some("open"),
        });
        assert_eq!(
            out,
            "<p>x <a class=\"ref ref-open\" href=\"/alice/demo/issues/3\" title=\"A &lt;b&gt;title&lt;/b&gt;\">#3</a> y</p>"
        );
        assert!(
            resolve("<a href=\"#slath-ref:bogus\">x</a>", None, |_| {
                Resolved::Text
            })
            .is_empty()
        );
    }
}
