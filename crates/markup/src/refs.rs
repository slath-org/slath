// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! References in Markdown: `#12`, `ns/repo#12`, `@user`, `@org/team`,
//! `~label`, `%milestone`, and commit IDs.
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
    /// A team of an organization.
    Team { org: String, team: String },
    /// A label of a repository (`~bug`, `~"needs review"`).
    Label {
        ns: String,
        repo: String,
        name: String,
    },
    /// A milestone of a repository (`%v1.0`, `%"Q3 cleanup"`).
    Milestone {
        ns: String,
        repo: String,
        title: String,
    },
}

/// Longest label name and milestone title in characters.
const MAX_LABEL: usize = 50;
const MAX_MILESTONE: usize = 100;

fn pct_encode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
            out.push(char::from(b));
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn pct_decode(s: &str) -> Option<String> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' {
            let h = std::str::from_utf8(b.get(i + 1..i + 3)?).ok()?;
            out.push(u8::from_str_radix(h, 16).ok()?);
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// A label name or milestone title as it may appear in a reference.
fn valid_title(s: &str, max: usize) -> bool {
    let n = s.chars().count();
    (1..=max).contains(&n) && s.trim() == s && !s.contains('"') && !s.chars().any(char::is_control)
}

/// `~bug` or `~"needs review"`.
fn sigil_text(sigil: char, name: &str) -> String {
    if valid_name(name) {
        format!("{sigil}{name}")
    } else {
        format!("{sigil}\"{name}\"")
    }
}

impl Ref {
    fn token(&self) -> String {
        match self {
            Self::Item { ns, repo, number } => format!("item:{ns}/{repo}:{number}"),
            Self::Commit { ns, repo, sha } => format!("commit:{ns}/{repo}:{sha}"),
            Self::Mention { name } => format!("user:{name}"),
            Self::Team { org, team } => format!("team:{org}/{team}"),
            Self::Label { ns, repo, name } => format!("label:{ns}/{repo}:{}", pct_encode(name)),
            Self::Milestone { ns, repo, title } => {
                format!("milestone:{ns}/{repo}:{}", pct_encode(title))
            }
        }
    }

    fn parse_token(t: &str) -> Option<Self> {
        let (kind, rest) = t.split_once(':')?;
        match kind {
            "user" => valid_name(rest).then(|| Self::Mention {
                name: rest.to_owned(),
            }),
            "team" => {
                let (org, team) = rest.split_once('/')?;
                (valid_name(org) && valid_name(team)).then(|| Self::Team {
                    org: org.to_owned(),
                    team: team.to_owned(),
                })
            }
            "label" | "milestone" => {
                let (path, enc) = rest.split_once(':')?;
                let (ns, repo) = path.split_once('/')?;
                let value = pct_decode(enc)?;
                if !valid_name(ns) || !valid_name(repo) {
                    return None;
                }
                if kind == "label" {
                    valid_title(&value, MAX_LABEL).then(|| Self::Label {
                        ns: ns.to_owned(),
                        repo: repo.to_owned(),
                        name: value,
                    })
                } else {
                    valid_title(&value, MAX_MILESTONE).then(|| Self::Milestone {
                        ns: ns.to_owned(),
                        repo: repo.to_owned(),
                        title: value,
                    })
                }
            }
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
            Self::Team { org, team } => format!("@{org}/{team}"),
            Self::Label { name, .. } => sigil_text('~', name),
            Self::Milestone { title, .. } => sigil_text('%', title),
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
            // @org/team
            if valid_name(name) && j < b.len() && b[j] == b'/' {
                let k = j + 1;
                let t_end = k + b[k..].iter().take_while(|c| is_name_byte(**c)).count();
                let team = text[k..t_end].trim_end_matches('.');
                let t_end = k + team.len();
                if valid_name(team) && end_ok(t_end) && (t_end == b.len() || b[t_end] != b'/') {
                    out.push(Found {
                        start: i,
                        end: t_end,
                        r: Ref::Team {
                            org: name.to_owned(),
                            team: team.to_owned(),
                        },
                    });
                    i = t_end;
                    continue;
                }
            }
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
        // ~label and %milestone in the document's repository
        if (b[i] == b'~' || b[i] == b'%')
            && let Some(c) = ctx
            && let Some((value, j)) = sigil_value(
                text,
                i + 1,
                if b[i] == b'~' {
                    MAX_LABEL
                } else {
                    MAX_MILESTONE
                },
            )
        {
            let r = if b[i] == b'~' {
                Ref::Label {
                    ns: c.ns.clone(),
                    repo: c.repo.clone(),
                    name: value,
                }
            } else {
                Ref::Milestone {
                    ns: c.ns.clone(),
                    repo: c.repo.clone(),
                    title: value,
                }
            };
            out.push(Found {
                start: i,
                end: j,
                r,
            });
            i = j;
            continue;
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

/// Words that close what follows them: `Fixes #12`, `closes: ns/repo#3`.
const CLOSING: [&str; 9] = [
    "close", "closes", "closed", "fix", "fixes", "fixed", "resolve", "resolves", "resolved",
];

/// The issues or merge requests a plain text (a commit message or a merge
/// request description) closes: each `#n` or `ns/repo#n` right after a
/// closing keyword (case-insensitive, optionally followed by a colon).
/// Deduplicated, in order, at most [`MAX_REFS`].
#[must_use]
pub fn closing_refs(text: &str, ctx: &RefContext) -> Vec<Ref> {
    let mut out: Vec<Ref> = Vec::new();
    for f in scan(text, Some(ctx)) {
        if !matches!(f.r, Ref::Item { .. }) || out.contains(&f.r) {
            continue;
        }
        let before = text[..f.start].trim_end_matches([' ', '\t']);
        // At least one space or tab between keyword and reference.
        if before.len() == f.start {
            continue;
        }
        let before = before.strip_suffix(':').unwrap_or(before);
        let word_start = before
            .rfind(|c: char| !c.is_ascii_alphabetic())
            .map_or(0, |i| i + 1);
        let word = &before[word_start..];
        if CLOSING.iter().any(|k| k.eq_ignore_ascii_case(word)) {
            out.push(f.r);
            if out.len() == MAX_REFS {
                break;
            }
        }
    }
    out
}

/// The value after `~` or `%` starting at `k`: a quoted title or a plain
/// name, with the end position.
fn sigil_value(text: &str, k: usize, max: usize) -> Option<(String, usize)> {
    let b = text.as_bytes();
    if b.get(k) == Some(&b'"') {
        let close = text[k + 1..].find('"')? + k + 1;
        let value = &text[k + 1..close];
        let end = close + 1;
        let end_ok = end == b.len() || !(b[end].is_ascii_alphanumeric() || b[end] == b'_');
        return (valid_title(value, max) && end_ok).then(|| (value.to_owned(), end));
    }
    let j = k + b[k..].iter().take_while(|c| is_name_byte(**c)).count();
    let name = text[k..j].trim_end_matches('.');
    let j = k + name.len();
    let end_ok = j == b.len() || !(b[j].is_ascii_alphanumeric() || b[j] == b'_' || b[j] == b'/');
    (valid_name(name) && name.chars().count() <= max && end_ok).then(|| (name.to_owned(), j))
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

    #[test]
    fn closing_keywords() {
        let ctx = RefContext {
            ns: "o".into(),
            repo: "r".into(),
        };
        let item = |ns: &str, repo: &str, number| Ref::Item {
            ns: ns.into(),
            repo: repo.into(),
            number,
        };
        assert_eq!(
            closing_refs("Fixes #1, closes: o/x#2 and RESOLVED #3\n\nfix #1", &ctx),
            vec![item("o", "r", 1), item("o", "x", 2), item("o", "r", 3)]
        );
        // Not after a keyword, a keyword inside a word, or no space.
        assert!(closing_refs("See #1; prefixes #2; fixes#3; unfixed #4", &ctx).is_empty());
        assert_eq!(closing_refs("Closes\t#5", &ctx), vec![item("o", "r", 5)]);
    }

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
            "path/x#12abc",
            "50% done",
            "a~b",
            "~\"unclosed",
            "~-x",
        ] {
            assert!(found(none).is_empty(), "{none}: {:?}", found(none));
        }
        assert_eq!(found("ask @acme/core."), ["team:acme/core"]);
        assert_eq!(
            found("~bug, ~\"needs review\" and %v1.0."),
            [
                "label:alice/demo:bug",
                "label:alice/demo:needs%20review",
                "milestone:alice/demo:v1.0"
            ]
        );
        assert_eq!(
            found("%\"Q3 cleanup\""),
            ["milestone:alice/demo:Q3%20cleanup"]
        );
        // Without a repository context, short forms aren't references.
        assert!(scan("#12 deadbeef1 ~bug %v1", None).is_empty());
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
            Ref::Team {
                org: "acme".into(),
                team: "core".into(),
            },
            Ref::Label {
                ns: "a".into(),
                repo: "b".into(),
                name: "needs review: \u{fc}".into(),
            },
            Ref::Milestone {
                ns: "a".into(),
                repo: "b".into(),
                title: "Q3 / cleanup".into(),
            },
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
            "label:a/b:%22x",
            "label:a/b:%0Ax",
            "label:a/b:",
            "team:a",
            "milestone:a/b:%ZZ",
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
