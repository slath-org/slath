// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! The documentation site: Slath's Markdown documents and its OpenAPI
//! reference rendered into static HTML pages.
//!
//! Each document is one page, titled by its first heading, with anchors
//! on every heading. Links between documents become links between pages
//! (`../user/webhooks.md#x` → `../user/webhooks.html#x`); a link to a
//! page, file, or anchor that doesn't exist is an error, so a broken link
//! fails the build. Every page gets the navigation of all sections and
//! links to the previous and next page. The output is deterministic and
//! uses only relative links, so it works under `/-/docs/` of any
//! instance and under `slath.org/docs/<version>/` alike.

use std::collections::{BTreeMap, BTreeSet};

use comrak::nodes::NodeValue;

/// A source file, by its path below `docs/` (`user/webhooks.md`).
#[derive(Debug, Clone)]
pub struct Source {
    pub path: String,
    pub bytes: Vec<u8>,
}

/// The theme: the page template and the files it uses (`_theme/…`).
///
/// The template's placeholders: `{{lang}}`, `{{title}}`, `{{root}}` (the
/// relative path to the site's root, `""` or `../`), `{{nav}}`,
/// `{{content}}`, `{{pager}}`, `{{version}}`, `{{switcher}}`, and
/// `{{scripts}}` (the switcher's script, published pages only).
#[derive(Debug, Clone)]
pub struct Theme {
    pub page: String,
    pub files: Vec<(String, Vec<u8>)>,
}

/// How the site is built.
#[derive(Debug, Clone)]
pub struct Options {
    /// The documented version, such as `0.3.0`.
    pub version: String,
    /// For slath.org: pages get the version switcher.
    pub published: bool,
}

/// The built site: files by path (`user/webhooks.html`, `_theme/docs.css`).
#[derive(Debug, Clone, Default)]
pub struct Site {
    pub files: BTreeMap<String, Vec<u8>>,
    /// Every page's anchors, for checking links into the site.
    pub anchors: BTreeMap<String, BTreeSet<String>>,
}

/// The sections, in navigation order: directory and title.
pub const SECTIONS: &[(&str, &str)] = &[
    ("user", "Using Slath"),
    ("admin", "Administration"),
    ("cli", "Command line"),
    ("dev", "Contributing"),
    ("api", "API reference"),
];

struct Page {
    /// Output path, `user/webhooks.html`.
    out: String,
    section: &'static str,
    title: String,
    content: String,
}

/// HTML-escapes text.
fn esc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}

/// A document's output path: `x.md` → `x.html`, `README.md` → `index.html`.
fn out_path(source: &str) -> String {
    match source.strip_suffix("README.md") {
        Some(dir) => format!("{dir}index.html"),
        None => source
            .strip_suffix(".md")
            .map_or_else(|| source.to_owned(), |s| format!("{s}.html")),
    }
}

/// Resolves `rel` against the directory of `from` (both below `docs/`);
/// `None` if it leaves the documentation.
fn resolve(from: &str, rel: &str) -> Option<String> {
    let mut parts: Vec<&str> = from.split('/').collect();
    parts.pop();
    for p in rel.split('/') {
        match p {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            p => parts.push(p),
        }
    }
    Some(parts.join("/"))
}

fn is_external(url: &str) -> bool {
    let u = url.to_ascii_lowercase();
    u.starts_with("//") || u.contains(':')
}

fn options() -> comrak::Options<'static> {
    let mut o = comrak::Options::default();
    o.extension.strikethrough = true;
    o.extension.table = true;
    o.extension.autolink = true;
    o.extension.tasklist = true;
    o.extension.footnotes = true;
    o.extension.header_id_prefix = Some(String::new());
    o.render.r#unsafe = false;
    o
}

/// The documents' own sanitizer: the Markdown allowlist, with heading
/// anchors as they are and links without `rel`.
fn sanitizer() -> ammonia::Builder<'static> {
    let mut b = crate::sanitizer_for_docs();
    b.link_rel(None)
        .add_tag_attributes("a", ["aria-label", "class"].iter())
        .attribute_filter(|element, attribute, value| match (element, attribute) {
            ("code", "class") => value.starts_with("language-").then(|| value.into()),
            ("a", "class") => (value == "anchor").then(|| value.into()),
            ("input", "type") => (value == "checkbox").then(|| value.into()),
            _ => Some(value.into()),
        });
    b
}

/// A link to check: from a document to a page or file, maybe an anchor.
struct Link {
    from: String,
    target: String,
    anchor: Option<String>,
}

/// Renders one document: (title, HTML, anchors), and its links.
fn render(
    path: &str,
    text: &str,
    links: &mut Vec<Link>,
) -> Result<(String, String, BTreeSet<String>), String> {
    // The SPDX header is an HTML comment: not part of the page.
    let mut text = text.trim_start();
    if let Some(rest) = text.strip_prefix("<!--") {
        text = rest.split_once("-->").map_or("", |(_, r)| r);
    }
    let opts = options();
    let arena = comrak::Arena::new();
    let root = comrak::parse_document(&arena, text, &opts);
    let mut anchorizer = comrak::Anchorizer::new();
    let mut anchors = BTreeSet::new();
    let mut title = None;
    for node in root.descendants() {
        let heading = matches!(node.data().value, NodeValue::Heading(_));
        if heading {
            let t = node.collect_text();
            anchors.insert(anchorizer.anchorize(&t));
            if title.is_none() {
                title = Some(t);
            }
            continue;
        }
        let mut data = node.data_mut();
        if let NodeValue::Link(link) = &mut data.value {
            let url = link.url.clone();
            if is_external(&url) {
                continue;
            }
            let (file, anchor) = match url.split_once('#') {
                Some((f, a)) => (f.to_owned(), Some(a.to_owned())),
                None => (url.clone(), None),
            };
            let target = if file.is_empty() {
                path.to_owned()
            } else {
                resolve(path, &file)
                    .ok_or_else(|| format!("{path}: the link {url} leaves the documentation"))?
            };
            links.push(Link {
                from: path.to_owned(),
                target: target.clone(),
                anchor: anchor.clone(),
            });
            if file.ends_with(".md") {
                let page = match file.strip_suffix("README.md") {
                    Some(dir) => format!("{dir}index.html"),
                    None => format!("{}.html", file.trim_end_matches(".md")),
                };
                link.url = match anchor {
                    Some(a) => format!("{page}#{a}"),
                    None => page,
                };
            }
        }
    }
    let title = title.ok_or_else(|| format!("{path}: no heading for the title"))?;
    let mut html = String::new();
    comrak::format_html(root, &opts, &mut html).map_err(|e| format!("{path}: {e}"))?;
    // Code blocks and tables scroll sideways on narrow screens: keyboard
    // users reach them as regions.
    let html = sanitizer()
        .clean(&html)
        .to_string()
        .replace("<pre>", "<pre tabindex=\"0\">")
        .replace(
            "<table>",
            "<div class=\"docs-table\" tabindex=\"0\" role=\"region\" aria-label=\"Table\"><table>",
        )
        .replace("</table>", "</table></div>");
    Ok((title, html, anchors))
}

/// Slug of an OpenAPI tag or operation.
fn slug(s: &str) -> String {
    let mut a = comrak::Anchorizer::new();
    a.anchorize(s)
}

/// The API reference: an index and one page per tag of the OpenAPI
/// document.
fn api_pages(openapi: &[u8]) -> Result<Vec<Page>, String> {
    let doc: serde_json::Value =
        serde_json::from_slice(openapi).map_err(|e| format!("api/openapi.json: {e}"))?;
    let mut tags: Vec<(String, String)> = doc["tags"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|t| {
            Some((
                t["name"].as_str()?.to_owned(),
                t["description"].as_str().unwrap_or("").to_owned(),
            ))
        })
        .collect();
    // Operations by tag, in path order.
    let mut ops: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let empty = serde_json::Map::new();
    let paths = doc["paths"].as_object().unwrap_or(&empty);
    for (path, item) in paths {
        for method in ["get", "post", "put", "patch", "delete"] {
            let Some(op) = item.get(method) else { continue };
            let tag = op["tags"][0].as_str().unwrap_or("other").to_owned();
            if !tags.iter().any(|(t, _)| *t == tag) {
                tags.push((tag.clone(), String::new()));
            }
            let id = op["operationId"].as_str().unwrap_or(path).to_owned();
            let mut h = format!(
                "<section class=\"api-op\" aria-labelledby=\"{id}\">\n<h2 id=\"{id}\"><code>{} {}</code></h2>\n",
                method.to_uppercase(),
                esc(path),
                id = esc(&slug(&id))
            );
            if let Some(s) = op["summary"].as_str() {
                h.push_str(&format!("<p>{}</p>\n", esc(s)));
            }
            if let Some(d) = op["description"].as_str() {
                h.push_str(&crate::render_markdown(
                    d,
                    crate::MarkdownOptions::default(),
                ));
            }
            let scopes: Vec<String> = op["security"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|s| s["token"].as_array())
                .flatten()
                .filter_map(|s| s.as_str().map(|s| format!("<code>{}</code>", esc(s))))
                .collect();
            if !scopes.is_empty() {
                h.push_str(&format!("<p>Token scopes: {}.</p>\n", scopes.join(", ")));
            }
            let params: Vec<&serde_json::Value> =
                op["parameters"].as_array().into_iter().flatten().collect();
            if !params.is_empty() {
                h.push_str("<div class=\"docs-table\" tabindex=\"0\" role=\"region\" aria-label=\"Parameters\"><table><thead><tr><th scope=\"col\">Parameter</th><th scope=\"col\">In</th><th scope=\"col\">Required</th></tr></thead><tbody>\n");
                for p in params {
                    h.push_str(&format!(
                        "<tr><td><code>{}</code></td><td>{}</td><td>{}</td></tr>\n",
                        esc(p["name"].as_str().unwrap_or("")),
                        esc(p["in"].as_str().unwrap_or("")),
                        if p["required"] == true { "yes" } else { "no" }
                    ));
                }
                h.push_str("</tbody></table></div>\n");
            }
            let body = &op["requestBody"]["content"]["application/json"]["schema"]["$ref"];
            if let Some(r) = body.as_str() {
                h.push_str(&format!(
                    "<p>Body: <code>{}</code></p>\n",
                    esc(r.rsplit('/').next().unwrap_or(r))
                ));
            }
            if let Some(rs) = op["responses"].as_object() {
                h.push_str("<dl class=\"api-responses\">\n");
                for (code, r) in rs {
                    h.push_str(&format!(
                        "<dt><code>{}</code></dt><dd>{}</dd>\n",
                        esc(code),
                        esc(r["description"].as_str().unwrap_or(""))
                    ));
                }
                h.push_str("</dl>\n");
            }
            h.push_str("</section>\n");
            ops.entry(tag).or_default().push(h);
        }
    }
    let mut pages = Vec::new();
    let mut index = String::from(
        "<h1 id=\"api-reference\">API reference</h1>\n<p>The REST API under <code>/api/v1</code>, by area. \
         The machine-readable description is <a href=\"openapi.json\">openapi.json</a> (OpenAPI 3.1).</p>\n<ul>\n",
    );
    for (tag, description) in &tags {
        let Some(list) = ops.get(tag) else { continue };
        let out = format!("api/{}.html", slug(tag));
        index.push_str(&format!(
            "<li><a href=\"{}.html\">{}</a>{}</li>\n",
            slug(tag),
            esc(tag),
            if description.is_empty() {
                String::new()
            } else {
                format!(": {}", esc(description))
            }
        ));
        let mut content = format!(
            "<h1 id=\"{}\">{}</h1>\n",
            esc(&slug(tag)),
            esc(&uppercase_first(tag))
        );
        if !description.is_empty() {
            content.push_str(&format!("<p>{}</p>\n", esc(description)));
        }
        content.push_str(&list.concat());
        pages.push(Page {
            out,
            section: "api",
            title: uppercase_first(tag),
            content,
        });
    }
    index.push_str("</ul>\n");
    pages.insert(
        0,
        Page {
            out: "api/index.html".to_owned(),
            section: "api",
            title: "API reference".to_owned(),
            content: index,
        },
    );
    Ok(pages)
}

fn uppercase_first(s: &str) -> String {
    let mut c = s.chars();
    c.next()
        .map(|f| f.to_uppercase().collect::<String>() + c.as_str())
        .unwrap_or_default()
}

/// The relative path from page `out` to the site's root.
fn root_of(out: &str) -> String {
    "../".repeat(out.matches('/').count())
}

/// Builds the site; the errors list every broken link and bad document.
pub fn build(sources: &[Source], theme: &Theme, opts: &Options) -> Result<Site, Vec<String>> {
    let mut errors = Vec::new();
    let mut links = Vec::new();
    let mut pages: Vec<Page> = Vec::new();
    let mut site = Site::default();
    let known: BTreeSet<&str> = sources.iter().map(|s| s.path.as_str()).collect();
    for s in sources {
        let Some(section) = SECTIONS
            .iter()
            .map(|(d, _)| *d)
            .find(|d| s.path.starts_with(&format!("{d}/")))
        else {
            continue;
        };
        if s.path == "api/openapi.json" {
            match api_pages(&s.bytes) {
                Ok(p) => pages.extend(p),
                Err(e) => errors.push(e),
            }
            site.files.insert(s.path.clone(), s.bytes.clone());
            continue;
        }
        if !s.path.ends_with(".md") {
            site.files.insert(s.path.clone(), s.bytes.clone());
            continue;
        }
        let Ok(text) = std::str::from_utf8(&s.bytes) else {
            errors.push(format!("{}: not UTF-8", s.path));
            continue;
        };
        match render(&s.path, text, &mut links) {
            Ok((title, content, anchors)) => {
                let out = out_path(&s.path);
                site.anchors.insert(out.clone(), anchors);
                pages.push(Page {
                    out,
                    section,
                    title,
                    content,
                });
            }
            Err(e) => errors.push(e),
        }
    }
    for l in &links {
        let page = out_path(&l.target);
        let exists = known.contains(l.target.as_str())
            || (l.target.ends_with('/')
                && known.contains(format!("{}README.md", l.target).as_str()));
        if !exists {
            errors.push(format!(
                "{}: the link to {} goes to no document",
                l.from, l.target
            ));
            continue;
        }
        if let Some(a) = &l.anchor
            && !site.anchors.get(&page).is_some_and(|ids| ids.contains(a))
        {
            errors.push(format!("{}: {} has no heading #{a}", l.from, l.target));
        }
    }
    // Navigation order: by section, the index page first, then by title.
    pages.sort_by(|a, b| {
        let rank = |p: &Page| SECTIONS.iter().position(|(d, _)| *d == p.section);
        let index = |p: &Page| !p.out.ends_with("/index.html");
        (rank(a), index(a), a.title.to_lowercase()).cmp(&(
            rank(b),
            index(b),
            b.title.to_lowercase(),
        ))
    });
    for p in &pages {
        site.anchors.entry(p.out.clone()).or_default();
    }
    let render_nav = |current: &str| -> String {
        let root = root_of(current);
        let mut nav = String::from(
            "<nav class=\"docs-nav\" id=\"docs-nav\" aria-label=\"Documentation\">\n<ul>\n",
        );
        for (dir, name) in SECTIONS {
            let in_section: Vec<&Page> = pages.iter().filter(|p| p.section == *dir).collect();
            if in_section.is_empty() {
                continue;
            }
            nav.push_str(&format!(
                "<li><span class=\"docs-nav-section\">{}</span>\n<ul>\n",
                esc(name)
            ));
            for p in in_section {
                let here = if p.out == current {
                    " aria-current=\"page\""
                } else {
                    ""
                };
                nav.push_str(&format!(
                    "<li><a href=\"{root}{}\"{here}>{}</a></li>\n",
                    p.out,
                    esc(&p.title)
                ));
            }
            nav.push_str("</ul></li>\n");
        }
        nav.push_str("</ul>\n</nav>");
        nav
    };
    let switcher = |root: &str| {
        if opts.published {
            format!(
                "<p class=\"docs-version\"><a href=\"{root}../versions.html\" data-versions=\"{root}../versions.json\" data-current=\"{}\">Version {}: other versions</a></p>",
                esc(&opts.version),
                esc(&opts.version)
            )
        } else {
            format!(
                "<p class=\"docs-version\">The documentation of Slath {}, the version running here.</p>",
                esc(&opts.version)
            )
        }
    };
    let fill = |out: &str, title: &str, content: &str, pager: &str| {
        let root = root_of(out);
        theme
            .page
            .replace("{{lang}}", "en")
            .replace("{{title}}", &esc(title))
            .replace("{{version}}", &esc(&opts.version))
            .replace("{{switcher}}", &switcher(&root))
            .replace(
                "{{scripts}}",
                &if opts.published {
                    format!("<script src=\"{root}_theme/docs.js\" defer></script>")
                } else {
                    String::new()
                },
            )
            .replace("{{nav}}", &render_nav(out))
            .replace("{{pager}}", pager)
            .replace("{{content}}", content)
            .replace("{{root}}", &root)
    };
    for (i, p) in pages.iter().enumerate() {
        let root = root_of(&p.out);
        let mut pager =
            String::from("<nav class=\"docs-pager\" aria-label=\"Previous and next page\">");
        if let Some(prev) = i.checked_sub(1).and_then(|k| pages.get(k)) {
            pager.push_str(&format!(
                "<a rel=\"prev\" href=\"{root}{}\">Previous: {}</a>",
                prev.out,
                esc(&prev.title)
            ));
        }
        if let Some(next) = pages.get(i + 1) {
            pager.push_str(&format!(
                "<a rel=\"next\" href=\"{root}{}\">Next: {}</a>",
                next.out,
                esc(&next.title)
            ));
        }
        pager.push_str("</nav>");
        site.files.insert(
            p.out.clone(),
            fill(&p.out, &p.title, &p.content, &pager).into_bytes(),
        );
    }
    // The start page: every section and its pages.
    let mut start = format!(
        "<h1 id=\"slath-documentation\">Slath documentation</h1>\n<p>Version {}.</p>\n",
        esc(&opts.version)
    );
    for (dir, name) in SECTIONS {
        let list: Vec<&Page> = pages.iter().filter(|p| p.section == *dir).collect();
        if list.is_empty() {
            continue;
        }
        start.push_str(&format!(
            "<h2 id=\"{}\">{}</h2>\n<ul>\n",
            slug(name),
            esc(name)
        ));
        for p in list {
            start.push_str(&format!(
                "<li><a href=\"{}\">{}</a></li>\n",
                p.out,
                esc(&p.title)
            ));
        }
        start.push_str("</ul>\n");
    }
    site.files.insert(
        "index.html".to_owned(),
        fill("index.html", "Slath documentation", &start, "").into_bytes(),
    );
    site.anchors.entry("index.html".to_owned()).or_default();
    for (name, bytes) in &theme.files {
        site.files.insert(format!("_theme/{name}"), bytes.clone());
    }
    if errors.is_empty() {
        Ok(site)
    } else {
        errors.sort();
        errors.dedup();
        Err(errors)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn src(path: &str, text: &str) -> Source {
        Source {
            path: path.to_owned(),
            bytes: text.as_bytes().to_vec(),
        }
    }

    fn theme() -> Theme {
        Theme {
            page: "<title>{{title}}</title><link href=\"{{root}}_theme/docs.css\">{{switcher}}{{nav}}<main>{{content}}</main>{{pager}}".to_owned(),
            files: vec![("docs.css".to_owned(), b"body{}".to_vec())],
        }
    }

    fn opts() -> Options {
        Options {
            version: "9.9.9".to_owned(),
            published: false,
        }
    }

    const API: &str = r#"{"openapi":"3.1.0","tags":[{"name":"issues","description":"Issues and comments"}],
      "paths":{"/api/v1/repos/{ns}/{repo}/issues":{"get":{"tags":["issues"],"operationId":"list_issues","summary":"Lists issues",
      "parameters":[{"name":"ns","in":"path","required":true}],"security":[{"token":["repo:read"]}],
      "responses":{"200":{"description":"The issues"}}}}}}"#;

    #[test]
    fn documents_become_linked_pages() {
        let site = build(
            &[
                src("user/a.md", "<!--\nSPDX\n-->\n\n# Alpha\n\n## Checking it\n\nSee [b](b.md#second-part), [admin](../admin/README.md), [here](#checking-it), [schema](../cli/x.schema.json), [web](https://example.org).\n"),
                src("user/b.md", "# Beta\n\n## Second part\n\n<script>alert(1)</script>\n"),
                src("admin/README.md", "# Admin\n"),
                src("cli/x.schema.json", "{}"),
                src("api/openapi.json", API),
                src("spec/secret.md", "# Not published\n"),
            ],
            &theme(),
            &opts(),
        )
        .unwrap();
        let a = String::from_utf8(site.files["user/a.html"].clone()).unwrap();
        assert!(a.contains("<title>Alpha</title>"), "{a}");
        assert!(!a.contains("SPDX"), "the header comment is gone");
        assert!(a.contains(r#"<a href="b.html#second-part">b</a>"#), "{a}");
        assert!(a.contains(r#"href="../admin/index.html""#), "{a}");
        assert!(a.contains(r##"href="#checking-it""##), "{a}");
        assert!(a.contains(r#"id="checking-it""#), "{a}");
        assert!(a.contains(r#"href="../_theme/docs.css""#), "{a}");
        assert!(a.contains("the version running here"), "{a}");
        assert!(a.contains(r#"aria-current="page""#), "{a}");
        let b = String::from_utf8(site.files["user/b.html"].clone()).unwrap();
        assert!(!b.contains("<script>"), "{b}");
        assert!(b.contains("Previous: Alpha"), "{b}");
        let api = String::from_utf8(site.files["api/issues.html"].clone()).unwrap();
        assert!(
            api.contains("<code>GET /api/v1/repos/{ns}/{repo}/issues</code>"),
            "{api}"
        );
        assert!(api.contains("<code>repo:read</code>"), "{api}");
        assert!(site.files.contains_key("api/openapi.json"));
        assert!(site.files.contains_key("cli/x.schema.json"));
        assert!(site.files.contains_key("_theme/docs.css"));
        assert!(site.files.contains_key("index.html"));
        assert!(
            !site.files.keys().any(|k| k.starts_with("spec/")),
            "planning docs aren't published"
        );
    }

    #[test]
    fn broken_links_and_anchors_fail_the_build() {
        let errors = build(
            &[
                src(
                    "user/a.md",
                    "# A\n\n[x](missing.md) [y](b.md#nope) [z](#nowhere) [w](../../ROADMAP.md)\n",
                ),
                src("user/b.md", "# B\n"),
                src("user/untitled.md", "no heading\n"),
            ],
            &theme(),
            &opts(),
        )
        .unwrap_err();
        let all = errors.join("\n");
        for needle in [
            "missing.md goes to no document",
            "#nope",
            "#nowhere",
            "leaves the documentation",
            "no heading for the title",
        ] {
            assert!(all.contains(needle), "{needle} in {all}");
        }
    }

    #[test]
    fn published_pages_have_the_switcher_and_the_output_is_deterministic() {
        let sources = [src("user/a.md", "# A\n"), src("user/b.md", "# B\n")];
        let p = Options {
            version: "0.3.0".to_owned(),
            published: true,
        };
        let one = build(&sources, &theme(), &p).unwrap();
        let two = build(&sources, &theme(), &p).unwrap();
        assert_eq!(one.files, two.files);
        let a = String::from_utf8(one.files["user/a.html"].clone()).unwrap();
        assert!(a.contains(r#"href="../../versions.html""#), "{a}");
        assert!(a.contains("Version 0.3.0: other versions"), "{a}");
    }
}
