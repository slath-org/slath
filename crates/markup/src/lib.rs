// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Markdown rendering, sanitizing, and syntax highlighting.
//!
//! Untrusted text becomes HTML only here. Markdown is rendered by comrak
//! (GitHub-flavored, raw HTML disabled) and then sanitized by ammonia with a
//! strict allowlist: no scripts, no event handlers, no inline styles, no
//! `data-*` or `hx-*` attributes, and links only to http(s), mailto, and
//! relative URLs. Highlighted code uses CSS classes only, so pages keep a
//! strict CSP. Text-direction control characters are made visible, so code
//! and text can't look different from what they are.
#![forbid(unsafe_code)]

mod highlight;

use std::collections::HashSet;

pub use highlight::{Highlighted, highlight};

/// Bumped whenever rendering output changes, so cached renders are discarded.
pub const RENDERER_VERSION: u32 = 1;

/// How many leading bytes [`is_binary`] inspects.
pub const BINARY_SNIFF: usize = 8 * 1024;

/// True if `data` looks binary: a NUL byte in its first 8 KiB.
#[must_use]
pub fn is_binary(data: &[u8]) -> bool {
    data[..data.len().min(BINARY_SNIFF)].contains(&0)
}

/// Unicode characters that change text direction (Trojan Source).
#[must_use]
pub const fn is_bidi_control(c: char) -> bool {
    matches!(c, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}' | '\u{200e}' | '\u{200f}' | '\u{061c}')
}

/// Replaces text-direction control characters with a visible `[U+XXXX]`.
#[must_use]
pub fn reveal_bidi(s: &str) -> std::borrow::Cow<'_, str> {
    if !s.chars().any(is_bidi_control) {
        return std::borrow::Cow::Borrowed(s);
    }
    let mut out = String::with_capacity(s.len() + 16);
    for c in s.chars() {
        if is_bidi_control(c) {
            out.push_str(&format!("[U+{:04X}]", u32::from(c)));
        } else {
            out.push(c);
        }
    }
    std::borrow::Cow::Owned(out)
}

/// Options for [`render_markdown`].
#[derive(Debug, Clone, Copy)]
pub struct MarkdownOptions {
    /// Allow images from other origins (`ui.external_images`). When false,
    /// such images become links.
    pub external_images: bool,
}

impl Default for MarkdownOptions {
    fn default() -> Self {
        Self {
            external_images: true,
        }
    }
}

fn comrak_options() -> comrak::Options<'static> {
    let mut o = comrak::Options::default();
    o.extension.strikethrough = true;
    o.extension.table = true;
    o.extension.autolink = true;
    o.extension.tasklist = true;
    o.extension.footnotes = true;
    // Raw HTML is never passed through; it renders as an omission marker.
    o.render.r#unsafe = false;
    o
}

fn is_external(url: &str) -> bool {
    let u = url.trim_start().to_ascii_lowercase();
    u.starts_with("//") || u.contains(':')
}

fn sanitizer() -> ammonia::Builder<'static> {
    let tags: HashSet<&str> = [
        "a",
        "abbr",
        "b",
        "blockquote",
        "br",
        "code",
        "dd",
        "del",
        "details",
        "div",
        "dl",
        "dt",
        "em",
        "h1",
        "h2",
        "h3",
        "h4",
        "h5",
        "h6",
        "hr",
        "i",
        "img",
        "input",
        "kbd",
        "li",
        "ol",
        "p",
        "picture",
        "pre",
        "s",
        "section",
        "source",
        "span",
        "strong",
        "sub",
        "summary",
        "sup",
        "table",
        "tbody",
        "td",
        "tfoot",
        "th",
        "thead",
        "tr",
        "ul",
    ]
    .into_iter()
    .collect();
    let mut b = ammonia::Builder::new();
    b.tags(tags)
        .clean_content_tags(["script", "style"].into_iter().collect())
        .generic_attributes(["title"].into_iter().collect())
        .add_tag_attributes("a", ["href", "id"].iter())
        .add_tag_attributes("img", ["src", "alt", "width", "height"].iter())
        .add_tag_attributes("source", ["srcset", "media", "type"].iter())
        .add_tag_attributes("input", ["type", "checked", "disabled"].iter())
        .add_tag_attributes("code", ["class"].iter())
        .add_tag_attributes("ol", ["start"].iter())
        .add_tag_attributes("td", ["align"].iter())
        .add_tag_attributes("th", ["align"].iter())
        .add_tag_attributes("li", ["id"].iter())
        .add_tag_attributes("sup", ["class"].iter())
        .add_tag_attributes("section", ["class"].iter())
        .add_tag_attributes("h1", ["id"].iter())
        .add_tag_attributes("h2", ["id"].iter())
        .add_tag_attributes("h3", ["id"].iter())
        .add_tag_attributes("h4", ["id"].iter())
        .add_tag_attributes("h5", ["id"].iter())
        .add_tag_attributes("h6", ["id"].iter())
        .url_schemes(["http", "https", "mailto"].into_iter().collect())
        .link_rel(Some("nofollow noopener noreferrer"))
        .strip_comments(true)
        .attribute_filter(|element, attribute, value| match (element, attribute) {
            // Only "language-…" classes on code and footnote classes survive.
            ("code", "class") => valid_class(value, "language-").then(|| value.into()),
            ("sup" | "section", "class") => {
                matches!(value, "footnote-ref" | "footnotes").then(|| value.into())
            }
            // Task list checkboxes only, always disabled.
            ("input", "type") => (value == "checkbox").then(|| value.into()),
            // Fragment IDs from content are prefixed, so they can't collide with the page's.
            (_, "id") => value.starts_with("user-content-").then(|| value.into()),
            _ => Some(value.into()),
        });
    b
}

fn valid_class(value: &str, prefix: &str) -> bool {
    value.strip_prefix(prefix).is_some_and(|rest| {
        !rest.is_empty()
            && rest.len() <= 40
            && rest
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"+#-_.".contains(&c))
    })
}

/// Sanitizes an HTML fragment with the Markdown allowlist. Rendering never
/// passes raw HTML through, so this is the second, independent layer.
#[must_use]
pub fn sanitize_html(html: &str) -> String {
    sanitizer().clean(html).to_string()
}

/// Renders untrusted Markdown to sanitized HTML.
#[must_use]
pub fn render_markdown(src: &str, opts: MarkdownOptions) -> String {
    let src = reveal_bidi(src);
    let options = comrak_options();
    let arena = comrak::Arena::new();
    let root = comrak::parse_document(&arena, &src, &options);
    if !opts.external_images {
        for node in root.descendants() {
            let mut data = node.data_mut();
            if let comrak::nodes::NodeValue::Image(link) = &data.value
                && is_external(&link.url)
            {
                let link = link.clone();
                data.value = comrak::nodes::NodeValue::Link(link);
            }
        }
    }
    let mut html = String::new();
    if comrak::format_html(root, &options, &mut html).is_err() {
        return String::new();
    }
    sanitizer().clean(&html).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn binary_detection() {
        assert!(is_binary(b"abc\0def"));
        assert!(!is_binary(b"plain text\n"));
        let mut late = vec![b'a'; BINARY_SNIFF];
        late.push(0);
        assert!(!is_binary(&late), "only the first 8 KiB count");
    }

    #[test]
    fn the_sanitizer_alone_strips_active_attributes() {
        let dirty = r#"<div hx-get="/x" hx-trigger="load" data-hx-post="/y" onclick="a()" style="x:y">t</div>
<a href="javascript:alert(1)" hx-nonce="n">l</a><img src="data:image/png;base64,AA" onerror="b()">
<svg><script>alert(1)</script></svg><form action="/z"><input type="text" name="q"></form>"#;
        let clean = sanitize_html(dirty);
        for bad in [
            "hx-",
            "data-",
            "onclick",
            "onerror",
            "style",
            "javascript",
            "data:",
            "<svg",
            "<script",
            "<form",
            "type=\"text\"",
        ] {
            assert!(!clean.contains(bad), "{bad} in {clean}");
        }
    }

    #[test]
    fn bidi_controls_are_revealed() {
        assert_eq!(reveal_bidi("a\u{202e}b"), "a[U+202E]b");
        assert_eq!(reveal_bidi("plain"), "plain");
    }

    #[test]
    fn external_images_can_become_links() {
        let md = "![badge](https://img.example/b.svg) ![local](/a/b.png)";
        let on = render_markdown(
            md,
            MarkdownOptions {
                external_images: true,
            },
        );
        assert!(
            on.contains("<img src=\"https://img.example/b.svg\""),
            "{on}"
        );
        let off = render_markdown(
            md,
            MarkdownOptions {
                external_images: false,
            },
        );
        assert!(!off.contains("img.example/b.svg\" alt"), "{off}");
        assert!(
            off.contains("<a href=\"https://img.example/b.svg\""),
            "{off}"
        );
        assert!(off.contains("<img src=\"/a/b.png\""), "{off}");
    }
}
