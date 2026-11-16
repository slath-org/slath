// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! The instance logo (A11Y-01, SEC-04): site administrators replace the
//! bundled logo for each theme; every page's header and favicon follow,
//! and a reset brings the bundled logo back. SVGs that could script,
//! handle events, or load anything else are refused; PNGs are re-encoded.
//! Nobody else can change it.

use slath_tests::http::{Client, csrf_token, multipart};
use slath_tests::server::{PASSWORD, TestServer};

const SVG: &[u8] = br##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24"><title>Acme</title><circle cx="12" cy="12" r="10" fill="#0a7"/></svg>"##;

fn png() -> Vec<u8> {
    let img = image::RgbaImage::from_pixel(800, 400, image::Rgba([200, 30, 30, 255]));
    let mut out = Vec::new();
    img.write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
        .unwrap();
    // A polyglot's tail: dropped by re-encoding.
    out.extend_from_slice(b"<script>alert(1)</script>");
    out
}

fn upload(c: &mut Client, parts: &[(&str, &str, &[u8])]) -> slath_tests::http::Resp {
    let csrf = csrf_token(&c.get("/-/admin/appearance").body).unwrap_or_default();
    let mut all: Vec<(&str, Option<&str>, &[u8])> = vec![("_csrf", None, csrf.as_bytes())];
    for (name, file, data) in parts {
        all.push((name, Some(file), data));
    }
    let (ct, body) = multipart(&all);
    c.request("POST", "/-/admin/appearance", &[], Some((&ct, body)))
}

/// The header's and favicons' logo URLs of a page.
fn logos(body: &str) -> Vec<String> {
    let mut v: Vec<String> = body
        .match_indices("/-/logos/")
        .map(|(i, _)| {
            body[i..]
                .split(['"', '\''])
                .next()
                .unwrap_or_default()
                .to_owned()
        })
        .collect();
    v.sort();
    v.dedup();
    v
}

#[test]
fn conf_a11y_01_administrators_replace_and_reset_the_logo() {
    let s = TestServer::start_with(&[]);
    s.create_user("boss", PASSWORD, true);
    s.create_user("alice", PASSWORD, false);
    let mut boss = s.login("boss", PASSWORD);
    let mut alice = s.login("alice", PASSWORD);
    assert!(logos(&s.client().get("/-/login").body).is_empty());

    let r = upload(
        &mut boss,
        &[("logo_light", "l.png", &png()), ("logo_dark", "d.svg", SVG)],
    );
    assert_eq!(r.status, 303, "{}", r.body);
    // Every page, signed in or not, uses them for header and favicon.
    for (who, c) in [("anonymous", &mut s.client()), ("alice", &mut alice)] {
        for path in ["/-/login", "/-/explore", "/alice", "/-/nothing-here"] {
            let r = c.get(path);
            if (300..400).contains(&r.status) {
                continue;
            }
            let found = logos(&r.body);
            assert_eq!(found.len(), 2, "{who} {path}: {found:?}");
            assert!(found.iter().any(|u| u.ends_with(".png")), "{who} {path}");
            assert!(found.iter().any(|u| u.ends_with(".svg")), "{who} {path}");
            assert!(
                r.body.contains(r#"<link rel="icon" type="image/png""#)
                    && r.body.contains(r#"<link rel="icon" type="image/svg+xml""#),
                "{who} {path}: favicons"
            );
        }
    }
    let page = s.client().get("/-/login").body;
    let urls = logos(&page);
    let png_url = urls.iter().find(|u| u.ends_with(".png")).unwrap();
    let svg_url = urls.iter().find(|u| u.ends_with(".svg")).unwrap();
    let got = s.client().get(png_url);
    assert_eq!(got.status, 200);
    assert_eq!(got.header("content-type"), Some("image/png"));
    assert_eq!(got.header("x-content-type-options"), Some("nosniff"));
    assert!(
        got.header("content-security-policy")
            .unwrap()
            .starts_with("sandbox")
    );
    assert!(
        !got.bytes.windows(8).any(|w| w == b"<script>"),
        "re-encoded"
    );
    let img = image::load_from_memory(&got.bytes).unwrap();
    assert_eq!((img.width(), img.height()), (512, 256));
    let got = s.client().get(svg_url);
    assert_eq!(got.header("content-type"), Some("image/svg+xml"));
    assert_eq!(got.bytes, SVG, "a checked SVG is served as uploaded");
    // The wrong extension, or another blob, isn't a logo.
    assert_eq!(s.client().get(&png_url.replace(".png", ".svg")).status, 404);
    assert_eq!(
        s.client()
            .get(&format!("/-/logos/{}.png", "0".repeat(64)))
            .status,
        404
    );

    // Reset: the bundled logo, everywhere.
    let csrf = csrf_token(&boss.get("/-/admin/appearance").body).unwrap();
    let r = boss.post_form("/-/admin/appearance/reset", &[("_csrf", &csrf)]);
    assert_eq!(r.status, 303, "{}", r.body);
    assert!(logos(&s.client().get("/-/login").body).is_empty());
    assert!(logos(&alice.get("/alice").body).is_empty());
    assert_eq!(
        s.db.query_one(
            "SELECT string_agg(action, ',' ORDER BY created_at) FROM audit_events WHERE action LIKE 'admin.logo.%'"
        ),
        "admin.logo.set,admin.logo.reset"
    );
}

#[test]
fn conf_sec_04_logo_svgs_that_could_script_or_load_are_refused() {
    let s = TestServer::start_with(&[]);
    s.create_user("boss", PASSWORD, true);
    let mut boss = s.login("boss", PASSWORD);
    for bad in [
        r#"<svg xmlns="http://www.w3.org/2000/svg"><script>alert(1)</script></svg>"#,
        r#"<svg xmlns="http://www.w3.org/2000/svg" onload="alert(1)"/>"#,
        r#"<svg xmlns="http://www.w3.org/2000/svg"><image href="https://evil.example/x.png"/></svg>"#,
        r#"<svg xmlns="http://www.w3.org/2000/svg"><style>@import url(https://evil.example/x.css)</style></svg>"#,
        r#"<svg xmlns="http://www.w3.org/2000/svg"><foreignObject><p xmlns="http://www.w3.org/1999/xhtml">x</p></foreignObject></svg>"#,
        r#"<!DOCTYPE svg [<!ENTITY x SYSTEM "file:///etc/passwd">]><svg xmlns="http://www.w3.org/2000/svg">&x;</svg>"#,
    ] {
        let r = upload(&mut boss, &[("logo_light", "x.svg", bad.as_bytes())]);
        assert_eq!(r.status, 422, "{bad}: {}", r.body);
        assert!(r.body.contains("There is a problem"), "{bad}");
    }
    let r = upload(
        &mut boss,
        &[("logo_dark", "x.gif", b"GIF89a\x01\x00\x01\x00")],
    );
    assert_eq!(r.status, 422);
    assert!(
        r.body.contains("Upload an SVG, PNG, or WebP image."),
        "{}",
        r.body
    );
    assert_eq!(
        s.db.query_one("SELECT count(*)::text FROM instance_logos"),
        "0"
    );
}

#[test]
fn conf_sec_04_only_site_administrators_change_the_logo() {
    let s = TestServer::start_with(&[]);
    s.create_user("alice", PASSWORD, false);
    let mut alice = s.login("alice", PASSWORD);
    assert_eq!(alice.get("/-/admin/appearance").status, 404);
    let csrf = csrf_token(&alice.get("/-/settings/profile").body).unwrap();
    let (ct, body) = multipart(&[
        ("_csrf", None, csrf.as_bytes()),
        ("logo_light", Some("x.svg"), SVG),
    ]);
    let r = alice.request("POST", "/-/admin/appearance", &[], Some((&ct, body)));
    assert_eq!(r.status, 404);
    let r = alice.post_form("/-/admin/appearance/reset", &[("_csrf", &csrf)]);
    assert_eq!(r.status, 404);
    assert_eq!(s.client().get("/-/admin/appearance").status, 404);
    assert_eq!(
        s.db.query_one("SELECT count(*)::text FROM instance_logos"),
        "0"
    );
}
