// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Attachments and avatars (ISSUE-02, SEC-04): active content is only ever
//! a download, private repositories' attachments are missing for everyone
//! else, IDs don't work across repositories, uploads need CSRF protection,
//! and avatars lose their metadata.

use slath_tests::http::{Client, create_repo, create_token, csrf_token, multipart};
use slath_tests::server::{PASSWORD, TestServer};

const SVG: &[u8] = b"<svg xmlns=\"http://www.w3.org/2000/svg\" onload=\"alert(1)\"><script>alert(2)</script></svg>";

fn png() -> Vec<u8> {
    let img = image::RgbImage::from_pixel(4, 4, image::Rgb([200, 10, 10]));
    let mut out = Vec::new();
    img.write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
        .unwrap();
    out
}

fn upload_api(
    c: &mut Client,
    token: &str,
    repo: &str,
    name: &str,
    data: &[u8],
) -> slath_tests::http::Resp {
    let (ct, body) = multipart(&[("file", Some(name), data)]);
    let auth = format!("Bearer {token}");
    c.request(
        "POST",
        &format!("/api/v1/repos/{repo}/attachments"),
        &[("authorization", &auth), ("cookie", "")],
        Some((&ct, body)),
    )
}

fn setup() -> (TestServer, Client, String) {
    let s = TestServer::start_full(&[], "[limits]\nattachment_max_bytes = \"64KiB\"\n");
    for u in ["alice", "bob"] {
        s.create_user(u, PASSWORD, false);
    }
    let mut a = s.login("alice", PASSWORD);
    create_repo(&mut a, "alice", "pub", "public");
    create_repo(&mut a, "alice", "priv", "private");
    let token = create_token(&mut a, "upload", &["issues:write", "repo:read"]);
    (s, a, token)
}

#[test]
fn conf_issue_02_svg_is_a_download_and_images_display_inline() {
    let (s, mut a, token) = setup();
    let r = upload_api(&mut a, &token, "alice/pub", "evil.svg", SVG);
    assert_eq!(r.status, 201, "{}", r.body);
    let j = r.json();
    assert_eq!(j["content_type"], "application/octet-stream");
    let url = j["url"].as_str().unwrap().to_owned();
    assert!(url.starts_with("/alice/pub/attachments/") && url.ends_with("/evil.svg"));
    assert!(j["markdown"].as_str().unwrap().starts_with("[evil.svg]("));

    let got = s.client().get(&url);
    assert_eq!(got.status, 200);
    assert_eq!(got.bytes, SVG);
    assert_eq!(got.header("content-type"), Some("application/octet-stream"));
    assert!(
        got.header("content-disposition")
            .unwrap()
            .starts_with("attachment;")
    );
    assert_eq!(got.header("x-content-type-options"), Some("nosniff"));
    assert!(
        got.header("content-security-policy")
            .unwrap()
            .contains("sandbox")
    );

    let r = upload_api(&mut a, &token, "alice/pub", "dot.png", &png());
    assert_eq!(r.status, 201, "{}", r.body);
    let j = r.json();
    assert_eq!(j["content_type"], "image/png");
    assert!(j["markdown"].as_str().unwrap().starts_with("![dot.png]("));
    let got = s.client().get(j["url"].as_str().unwrap());
    assert_eq!(got.header("content-type"), Some("image/png"));
    assert!(
        got.header("content-disposition")
            .unwrap()
            .starts_with("inline;")
    );

    // An HTML file renamed to .png is still a download.
    let r = upload_api(
        &mut a,
        &token,
        "alice/pub",
        "x.png",
        b"<html><script>1</script>",
    );
    assert_eq!(r.json()["content_type"], "application/octet-stream");

    // Too large and empty uploads are refused.
    let r = upload_api(
        &mut a,
        &token,
        "alice/pub",
        "big.bin",
        &vec![7u8; 65 * 1024],
    );
    assert_eq!(r.status, 413, "{}", r.body);
    let r = upload_api(&mut a, &token, "alice/pub", "empty", b"");
    assert_eq!(r.status, 422, "{}", r.body);
}

#[test]
fn conf_rbac_02_private_attachments_are_missing_for_others() {
    let (s, mut a, token) = setup();
    let r = upload_api(&mut a, &token, "alice/priv", "plan.txt", b"secret plan");
    assert_eq!(r.status, 201, "{}", r.body);
    let url = r.json()["url"].as_str().unwrap().to_owned();
    let id = r.json()["id"].as_str().unwrap().to_owned();

    assert_eq!(a.get(&url).status, 200);
    assert_eq!(
        a.get(&url).header("cache-control"),
        Some("private, no-store")
    );
    let mut bob = s.login("bob", PASSWORD);
    assert_eq!(bob.get(&url).status, 404);
    assert_eq!(s.client().get(&url).status, 404);
    // The ID doesn't work through another (readable) repository.
    assert_eq!(
        a.get(&format!("/alice/pub/attachments/{id}/plan.txt"))
            .status,
        404
    );
    // bob can't upload into it either, and anonymous uploads are refused.
    let bob_token = create_token(&mut bob, "b", &["issues:write"]);
    assert_eq!(
        upload_api(&mut bob, &bob_token, "alice/priv", "x", b"x").status,
        404
    );
    let (ct, body) = multipart(&[("file", Some("x"), b"x")]);
    assert_eq!(
        s.client()
            .request(
                "POST",
                "/api/v1/repos/alice/pub/attachments",
                &[],
                Some((&ct, body))
            )
            .status,
        401
    );
    // A token without issues:write can't upload.
    let read_only = create_token(&mut a, "ro", &["repo:read"]);
    assert_eq!(
        upload_api(&mut a, &read_only, "alice/pub", "x", b"x").status,
        403
    );
}

#[test]
fn conf_sec_04_page_uploads_need_the_csrf_header() {
    let (_s, mut a, _) = setup();
    let page = a.get("/alice/pub/issues/new");
    let token = csrf_token(&page.body).unwrap();
    let (ct, body) = multipart(&[("file", Some("n.txt"), b"note")]);
    let r = a.request(
        "POST",
        "/alice/pub/-/attachments",
        &[],
        Some((&ct, body.clone())),
    );
    assert_eq!(r.status, 403, "no token");
    let r = a.request(
        "POST",
        "/alice/pub/-/attachments",
        &[("x-csrf-token", "forged")],
        Some((&ct, body.clone())),
    );
    assert_eq!(r.status, 403);
    let r = a.request(
        "POST",
        "/alice/pub/-/attachments",
        &[("x-csrf-token", &token)],
        Some((&ct, body)),
    );
    assert_eq!(r.status, 201, "{}", r.body);
    assert!(r.json()["url"].as_str().unwrap().ends_with("/n.txt"));
}

#[test]
fn conf_sec_07_avatars_are_reencoded_without_metadata() {
    let (s, mut a, _) = setup();
    // A JPEG with GPS data in an EXIF segment.
    let img = image::RgbImage::from_pixel(300, 200, image::Rgb([20, 120, 220]));
    let mut jpeg = Vec::new();
    img.write_to(
        &mut std::io::Cursor::new(&mut jpeg),
        image::ImageFormat::Jpeg,
    )
    .unwrap();
    let exif = b"Exif\0\0MM\0*\0\0\0\x08GPSLatitude 48.1371 N";
    let len = u16::try_from(exif.len() + 2).unwrap().to_be_bytes();
    let mut tagged = jpeg[..2].to_vec();
    tagged.extend_from_slice(&[0xFF, 0xE1, len[0], len[1]]);
    tagged.extend_from_slice(exif);
    tagged.extend_from_slice(&jpeg[2..]);

    let page = a.get("/-/settings/profile");
    assert!(page.body.contains("/-/avatars/alice.svg"));
    let csrf = csrf_token(&page.body).unwrap();
    let (ct, body) = multipart(&[
        ("_csrf", None, csrf.as_bytes()),
        ("avatar", Some("me.jpg"), &tagged),
    ]);
    let r = a.request("POST", "/-/settings/avatar", &[], Some((&ct, body)));
    assert_eq!(r.status, 303, "{}", r.body);
    let profile = s.client().get("/alice");
    let start = profile.body.find("/-/avatars/").unwrap();
    let src = &profile.body[start..start + "/-/avatars/".len() + 64 + 4];
    assert!(src.ends_with(".png"), "{src}");
    let got = s.client().get(src);
    assert_eq!(got.status, 200);
    assert_eq!(got.header("content-type"), Some("image/png"));
    assert!(!got.bytes.windows(3).any(|w| w == b"GPS"));
    assert!(!got.bytes.windows(4).any(|w| w == b"Exif"));

    // Without the CSRF field the upload is refused; SVG is no avatar.
    let (ct, body) = multipart(&[("avatar", Some("me.png"), &png())]);
    assert_eq!(
        a.request("POST", "/-/settings/avatar", &[], Some((&ct, body)))
            .status,
        403
    );
    let csrf = csrf_token(&a.get("/-/settings/profile").body).unwrap();
    let (ct, body) = multipart(&[
        ("_csrf", None, csrf.as_bytes()),
        ("avatar", Some("x.svg"), SVG),
    ]);
    let r = a.request("POST", "/-/settings/avatar", &[], Some((&ct, body)));
    assert_eq!(r.status, 422);
    assert!(r.body.contains("Upload a PNG, JPEG, or WebP image."));

    // Attachment blobs are never served as avatars.
    let token = create_token(&mut a, "t", &["issues:write"]);
    let up = upload_api(&mut a, &token, "alice/priv", "p.png", &png());
    let sha = s.db.query_one(&format!(
        "SELECT blob_sha256 FROM attachments WHERE id = '{}'",
        up.json()["id"].as_str().unwrap()
    ));
    assert_eq!(s.client().get(&format!("/-/avatars/{sha}.png")).status, 404);

    // Removing it brings the identicon back.
    let csrf = csrf_token(&a.get("/-/settings/profile").body).unwrap();
    let (ct, body) = multipart(&[("_csrf", None, csrf.as_bytes()), ("remove", None, b"1")]);
    assert_eq!(
        a.request("POST", "/-/settings/avatar", &[], Some((&ct, body)))
            .status,
        303
    );
    assert!(
        s.client()
            .get("/alice")
            .body
            .contains("/-/avatars/alice.svg")
    );
}
