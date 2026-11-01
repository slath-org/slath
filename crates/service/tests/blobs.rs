// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! The blob store: attachments, avatars without metadata, reference counts,
//! and the collector's grace period.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use slath_core::authz::{Action, Visibility};
use slath_service::avatars::{AvatarOwner, reencode};
use slath_service::blobs::Staged;
use slath_service::repos::NewRepoInput;
use sqlx::PgPool;

/// A JPEG with an EXIF segment holding GPS coordinates and a camera name.
fn jpeg_with_exif() -> Vec<u8> {
    let img = image::RgbImage::from_fn(320, 200, |x, y| {
        image::Rgb([(x % 256) as u8, (y % 256) as u8, 128])
    });
    let mut plain = Vec::new();
    img.write_to(
        &mut std::io::Cursor::new(&mut plain),
        image::ImageFormat::Jpeg,
    )
    .unwrap();
    let payload =
        b"Exif\0\0MM\0*\0\0\0\x08GPSLatitude 52.5200 N GPSLongitude 13.4050 E SecretCam 3000";
    let len = u16::try_from(payload.len() + 2).unwrap().to_be_bytes();
    let mut out = plain[..2].to_vec(); // SOI
    out.extend_from_slice(&[0xFF, 0xE1, len[0], len[1]]);
    out.extend_from_slice(payload);
    out.extend_from_slice(&plain[2..]);
    out
}

fn contains(hay: &[u8], needle: &[u8]) -> bool {
    hay.windows(needle.len()).any(|w| w == needle)
}

async fn stage(svc: &slath_service::Services, data: &[u8]) -> Staged {
    let mut w = svc.attachment_writer().await.unwrap();
    w.write(data).await.unwrap();
    w.finish().await.unwrap()
}

async fn refcount(pool: &PgPool, sha: &str) -> Option<i32> {
    sqlx::query_scalar::<_, i32>("SELECT refcount FROM blobs WHERE sha256 = $1")
        .bind(sha)
        .fetch_optional(pool)
        .await
        .unwrap()
}

#[test]
fn avatars_keep_no_metadata() {
    let src = jpeg_with_exif();
    assert!(contains(&src, b"GPSLatitude"));
    let png = reencode(&src).unwrap();
    assert!(png.starts_with(b"\x89PNG\r\n\x1a\n"));
    for marker in [&b"GPS"[..], b"Exif", b"SecretCam", b"eXIf", b"tEXt"] {
        assert!(!contains(&png, marker), "{marker:?} survived");
    }
    let img = image::load_from_memory(&png).unwrap();
    assert_eq!((img.width(), img.height()), (256, 256));
    // Only raster images are accepted.
    assert!(reencode(b"<svg xmlns='http://www.w3.org/2000/svg'/>").is_err());
    assert!(reencode(b"GIF89a....").is_err());
}

#[sqlx::test(migrations = "../../migrations")]
async fn unused_blobs_are_kept_for_the_grace_period(pool: PgPool) {
    let env = common::services(pool.clone(), "[limits]\nblob_grace_period = \"1h\"\n");
    let svc = &env.svc;
    let (_, alice) = common::user(svc, "alice").await;
    svc.set_avatar(&alice, AvatarOwner::Me, Some(jpeg_with_exif()))
        .await
        .unwrap();
    let url = svc.avatar_url("alice").await;
    let sha = url
        .strip_prefix("/-/avatars/")
        .and_then(|s| s.strip_suffix(".png"))
        .unwrap()
        .to_owned();
    assert_eq!(refcount(&pool, &sha).await, Some(1));
    let path = env.dir.path().join("data/blobs").join(&sha[..2]).join(&sha);
    let stored = std::fs::read(&path).unwrap();
    assert!(!contains(&stored, b"GPS"));

    // Removing the avatar releases the blob, but the file stays during the
    // grace period.
    svc.set_avatar(&alice, AvatarOwner::Me, None).await.unwrap();
    assert_eq!(refcount(&pool, &sha).await, Some(0));
    assert_eq!(svc.avatar_url("alice").await, "/-/avatars/alice.svg");
    assert!(
        svc.avatar_file(&sha).await.is_err(),
        "unused avatars aren't served"
    );
    let c = svc.collect_blobs().await.unwrap();
    assert_eq!(c.blobs, 0);
    assert!(path.exists());

    // Reuse within the grace period revives it.
    svc.set_avatar(&alice, AvatarOwner::Me, Some(jpeg_with_exif()))
        .await
        .unwrap();
    assert_eq!(refcount(&pool, &sha).await, Some(1));
    svc.set_avatar(&alice, AvatarOwner::Me, None).await.unwrap();

    // After the grace period it is deleted, row and file.
    sqlx::query(
        "UPDATE blobs SET unreferenced_since = now() - interval '2 hours' WHERE sha256 = $1",
    )
    .bind(&sha)
    .execute(&pool)
    .await
    .unwrap();
    let c = svc.collect_blobs().await.unwrap();
    assert_eq!(c.blobs, 1);
    assert_eq!(refcount(&pool, &sha).await, None);
    assert!(!path.exists());
}

#[sqlx::test(migrations = "../../migrations")]
async fn orphaned_files_are_deleted_after_the_grace_period(pool: PgPool) {
    let env = common::services(pool.clone(), "[limits]\nblob_grace_period = \"1h\"\n");
    let svc = &env.svc;
    let root = env.dir.path().join("data/blobs");
    let old = format!("ab{}", "1".repeat(62));
    let fresh = format!("cd{}", "2".repeat(62));
    for sha in [&old, &fresh] {
        std::fs::create_dir_all(root.join(&sha[..2])).unwrap();
        std::fs::write(root.join(&sha[..2]).join(sha), b"x").unwrap();
    }
    let f = std::fs::File::options()
        .write(true)
        .open(root.join("ab").join(&old))
        .unwrap();
    f.set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(7200))
        .unwrap();
    let c = svc.collect_blobs().await.unwrap();
    assert_eq!(c.orphans, 1);
    assert!(!root.join("ab").join(&old).exists());
    assert!(root.join("cd").join(&fresh).exists(), "recent uploads stay");
}

#[sqlx::test(migrations = "../../migrations")]
async fn attachments_are_bound_to_their_repository(pool: PgPool) {
    let env = common::services(pool.clone(), "");
    let svc = &env.svc;
    let (_, alice) = common::user(svc, "alice").await;
    let (_, bob) = common::user(svc, "bob").await;
    for (name, vis) in [("a", Visibility::Private), ("b", Visibility::Public)] {
        svc.create_repo(
            &alice,
            NewRepoInput {
                namespace: "alice".into(),
                name: name.into(),
                description: String::new(),
                visibility: vis,
                default_branch: "main".into(),
            },
        )
        .await
        .unwrap();
    }
    let a = svc
        .attachment_upload_access(&alice, "alice", "a")
        .await
        .unwrap();
    let att = svc
        .create_attachment(&alice, &a, "../notes.svg", stage(svc, b"<svg/>").await)
        .await
        .unwrap();
    assert_eq!(att.filename, "notes.svg");
    assert_eq!(att.content_type, "application/octet-stream");
    // Same content again shares the blob.
    let again = svc
        .create_attachment(&alice, &a, "copy.svg", stage(svc, b"<svg/>").await)
        .await
        .unwrap();
    assert_eq!(again.blob_sha256, att.blob_sha256);
    assert_eq!(refcount(&pool, &att.blob_sha256).await, Some(2));

    assert!(svc.attachment(&alice, "alice", "a", att.id).await.is_ok());
    // Another repository's ID, another reader, an anonymous reader: missing.
    assert!(svc.attachment(&alice, "alice", "b", att.id).await.is_err());
    assert!(svc.attachment(&bob, "alice", "a", att.id).await.is_err());
    assert!(
        svc.attachment(&slath_core::authz::Subject::Anonymous, "alice", "a", att.id)
            .await
            .is_err()
    );
    // bob can't upload to a repository he can't see.
    assert!(
        svc.attachment_upload_access(&bob, "alice", "a")
            .await
            .is_err()
    );

    // Deleting the repository releases the blob.
    let ra = svc
        .repo_access(&alice, "alice", "a", Action::RepoRead)
        .await
        .unwrap();
    sqlx::query("DELETE FROM repositories WHERE id = $1")
        .bind(ra.repo.id.as_uuid())
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(refcount(&pool, &att.blob_sha256).await, Some(0));
}
