// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Checking an uploaded SVG image (an instance logo) before it's served.
//!
//! Only drawing survives: an allowlist of shape, gradient, and grouping
//! elements and their presentation attributes. Scripts, event handlers,
//! styles, foreign content, animation, links, and any reference outside
//! the file (`href`s and `url()`s that aren't `#fragment`s) are refused,
//! as are DTDs and entities. The file is refused whole rather than
//! rewritten, so what's served is exactly what was checked.

/// Elements a logo may use.
const ELEMENTS: &[&str] = &[
    "svg",
    "g",
    "defs",
    "symbol",
    "use",
    "title",
    "desc",
    "path",
    "rect",
    "circle",
    "ellipse",
    "line",
    "polyline",
    "polygon",
    "linearGradient",
    "radialGradient",
    "stop",
    "clipPath",
    "mask",
    "text",
    "tspan",
];

/// Attributes a logo may use (without a namespace, besides `href`).
const ATTRIBUTES: &[&str] = &[
    "id",
    "class",
    "version",
    "width",
    "height",
    "viewBox",
    "preserveAspectRatio",
    "x",
    "y",
    "x1",
    "y1",
    "x2",
    "y2",
    "cx",
    "cy",
    "r",
    "rx",
    "ry",
    "fx",
    "fy",
    "d",
    "points",
    "transform",
    "fill",
    "fill-opacity",
    "fill-rule",
    "stroke",
    "stroke-width",
    "stroke-opacity",
    "stroke-linecap",
    "stroke-linejoin",
    "stroke-miterlimit",
    "stroke-dasharray",
    "stroke-dashoffset",
    "opacity",
    "clip-path",
    "clip-rule",
    "mask",
    "color",
    "offset",
    "stop-color",
    "stop-opacity",
    "gradientUnits",
    "gradientTransform",
    "spreadMethod",
    "clipPathUnits",
    "maskUnits",
    "maskContentUnits",
    "font-family",
    "font-size",
    "font-weight",
    "font-style",
    "text-anchor",
    "dominant-baseline",
    "letter-spacing",
    "dx",
    "dy",
    "role",
    "aria-label",
    "aria-hidden",
    "focusable",
    "shape-rendering",
    "vector-effect",
    "paint-order",
    "visibility",
    "display",
];

const SVG_NS: &str = "http://www.w3.org/2000/svg";
const XLINK_NS: &str = "http://www.w3.org/1999/xlink";

/// A value that refers only inside the file: no `url(...)` but `url(#id)`.
fn local_refs_only(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    let mut rest = lower.as_str();
    while let Some(i) = rest.find("url(") {
        let inner = rest[i + 4..].trim_start().trim_start_matches(['"', '\'']);
        if !inner.starts_with('#') {
            return false;
        }
        rest = &rest[i + 4..];
    }
    !lower.contains("javascript:") && !lower.contains("data:")
}

/// Checks an SVG file; `Err` says what isn't allowed.
pub fn check_logo(data: &[u8]) -> Result<(), String> {
    let text = std::str::from_utf8(data).map_err(|_| "The SVG isn't UTF-8 text.".to_owned())?;
    if text.contains("<!DOCTYPE") || text.contains("<!ENTITY") {
        return Err("The SVG has a DOCTYPE or entities.".into());
    }
    let doc =
        roxmltree::Document::parse(text).map_err(|e| format!("The SVG isn't valid XML: {e}."))?;
    let root = doc.root_element();
    if root.tag_name().name() != "svg" || root.tag_name().namespace() != Some(SVG_NS) {
        return Err("The file isn't an SVG image.".into());
    }
    for node in doc.descendants() {
        if node.is_pi() {
            return Err("The SVG has processing instructions.".into());
        }
        if !node.is_element() {
            continue;
        }
        let name = node.tag_name();
        if name.namespace() != Some(SVG_NS) || !ELEMENTS.contains(&name.name()) {
            return Err(format!("<{}> isn't allowed in a logo.", name.name()));
        }
        for a in node.attributes() {
            let (ns, an) = (a.namespace(), a.name());
            let href = an == "href" && (ns.is_none() || ns == Some(XLINK_NS));
            if href {
                if !a.value().starts_with('#') {
                    return Err("Links and references to other files aren't allowed.".into());
                }
                continue;
            }
            if an == "style" && ns.is_none() {
                let v = a.value();
                if v.contains('@') || v.contains('\\') || !local_refs_only(v) {
                    return Err("The style attribute refers outside the file.".into());
                }
                continue;
            }
            if ns.is_some() || !ATTRIBUTES.contains(&an) {
                return Err(format!("The attribute {an} isn't allowed in a logo."));
            }
            if !local_refs_only(a.value()) {
                return Err("References to other files aren't allowed.".into());
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::check_logo;

    const OK: &str = r##"<?xml version="1.0" encoding="UTF-8"?>
<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink" viewBox="0 0 24 24" width="24" height="24">
  <title>Acme</title>
  <defs><linearGradient id="g"><stop offset="0" stop-color="#f00"/></linearGradient></defs>
  <g fill="url(#g)"><path d="M0 0h24v24H0z"/><circle cx="12" cy="12" r="4"/></g>
  <use xlink:href="#g"/><use href="#g"/>
</svg>"##;

    fn wrap(inner: &str) -> String {
        format!(
            r#"<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink" viewBox="0 0 1 1">{inner}</svg>"#
        )
    }

    #[test]
    fn plain_drawings_pass() {
        assert_eq!(check_logo(OK.as_bytes()), Ok(()));
        let styled = wrap(r#"<path d="M0 0" style="fill:#123456;stroke:url(#g)"/>"#);
        assert_eq!(check_logo(styled.as_bytes()), Ok(()));
        for bundled in ["logo-light.svg", "logo-dark.svg"] {
            let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../web/src/brand/");
            let svg = std::fs::read(format!("{path}{bundled}")).unwrap();
            assert_eq!(check_logo(&svg), Ok(()), "{bundled}");
        }
    }

    #[test]
    fn scripts_handlers_and_outside_references_are_refused() {
        for bad in [
            "<script>alert(1)</script>",
            r#"<path d="M0 0" onload="alert(1)"/>"#,
            r#"<g onclick="alert(1)"/>"#,
            "<style>@import url(https://x.example/a.css)</style>",
            r#"<foreignObject><div xmlns="http://www.w3.org/1999/xhtml">x</div></foreignObject>"#,
            r#"<image href="https://x.example/a.png"/>"#,
            r#"<use href="https://x.example/a.svg#x"/>"#,
            r#"<use xlink:href="data:image/svg+xml,&lt;svg/&gt;"/>"#,
            r#"<a href="javascript:alert(1)"><path d="M0 0"/></a>"#,
            r#"<path d="M0 0" fill="url(https://x.example/#g)"/>"#,
            r#"<path d="M0 0" style="fill:url(https://x.example/#g)"/>"#,
            r#"<path d="M0 0" style="background:u\72l(https://x.example)"/>"#,
            r#"<path d="M0 0" style="@import 'https://x.example'"/>"#,
            r#"<animate attributeName="href" to="javascript:alert(1)"/>"#,
            r#"<set attributeName="onmouseover" to="alert(1)"/>"#,
            r#"<iframe src="https://x.example"/>"#,
            "<?xml-stylesheet href=\"https://x.example/a.css\"?>",
        ] {
            assert!(check_logo(wrap(bad).as_bytes()).is_err(), "{bad}");
        }
    }

    #[test]
    fn entities_html_and_garbage_are_refused() {
        for bad in [
            r#"<!DOCTYPE svg [<!ENTITY x SYSTEM "file:///etc/passwd">]><svg xmlns="http://www.w3.org/2000/svg">&x;</svg>"#,
            "<html><body>x</body></html>",
            "<svg>no namespace</svg>",
            "not xml at all",
            "<svg xmlns=\"http://www.w3.org/2000/svg\">",
        ] {
            assert!(check_logo(bad.as_bytes()).is_err(), "{bad}");
        }
        assert!(check_logo(&[0xff, 0xfe, 0x00]).is_err());
    }
}
