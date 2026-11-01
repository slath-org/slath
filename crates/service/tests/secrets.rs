// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Secrets at rest (SEC-06): row binding and master key rotation.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use slath_auth::MasterKey;
use sqlx::PgPool;

async fn store(svc: &slath_service::Services, user: &str, plain: &[u8]) {
    let s = svc
        .seal(plain, "totp_credentials", "secret_enc", user)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO totp_credentials (user_id, secret_enc, key_id) VALUES ($1::uuid, $2, $3)",
    )
    .bind(user)
    .bind(&s.ciphertext)
    .bind(&s.key_id)
    .execute(&svc.pool)
    .await
    .unwrap();
}

async fn rows(pool: &PgPool) -> Vec<(String, Vec<u8>, String)> {
    sqlx::query_as(
        "SELECT user_id::text, secret_enc, key_id FROM totp_credentials ORDER BY user_id",
    )
    .fetch_all(pool)
    .await
    .unwrap()
}

#[sqlx::test(migrations = "../../migrations")]
async fn conf_sec_06_swapped_ciphertext_fails_to_decrypt(pool: PgPool) {
    let env = common::services(pool.clone(), "");
    let (a, _) = common::user(&env.svc, "alice").await;
    let (b, _) = common::user(&env.svc, "bob").await;
    store(&env.svc, &a.to_string(), b"seed-a").await;
    store(&env.svc, &b.to_string(), b"seed-b").await;
    let r = rows(&pool).await;
    for (id, ct, key) in &r {
        let plain = env
            .svc
            .open(ct, key, "totp_credentials", "secret_enc", id)
            .unwrap();
        assert!(plain.expose().starts_with(b"seed-"));
    }
    // Alice's ciphertext copied into Bob's row is rejected.
    assert!(
        env.svc
            .open(&r[0].1, &r[0].2, "totp_credentials", "secret_enc", &r[1].0)
            .is_err()
    );
    // The dump contains no plaintext.
    let dump: Vec<u8> = r.iter().flat_map(|(_, ct, _)| ct.clone()).collect();
    assert!(!dump.windows(6).any(|w| w == b"seed-a" || w == b"seed-b"));
}

#[sqlx::test(migrations = "../../migrations")]
async fn conf_sec_06_rotation_reencrypts_idempotently_and_resumes(pool: PgPool) {
    let old = MasterKey::generate().unwrap();
    let new = MasterKey::generate().unwrap();
    let before = common::services_with_key(pool.clone(), "", &old, &[]);
    let mut users = Vec::new();
    for i in 0..7 {
        let (id, _) = common::user(&before.svc, &format!("user{i}")).await;
        store(&before.svc, &id.to_string(), format!("seed-{i}").as_bytes()).await;
        users.push(id);
    }

    // Without the old key nothing can be re-encrypted, and nothing is retired.
    let only_new = common::services_with_key(pool.clone(), "", &new, &[]);
    let report = only_new.svc.rotate_keys(3).await.unwrap();
    assert_eq!(report.reencrypted, 0);
    assert_eq!(report.failed.len(), 7);
    assert!(report.retired.is_empty());

    // An interrupted rotation: some rows are already under the new key.
    let after = common::services_with_key(pool.clone(), "", &new, &[old]);
    for (id, ct, key) in rows(&pool).await.into_iter().take(3) {
        let plain = after
            .svc
            .open(&ct, &key, "totp_credentials", "secret_enc", &id)
            .unwrap();
        let s = after
            .svc
            .seal(plain.expose(), "totp_credentials", "secret_enc", &id)
            .await
            .unwrap();
        sqlx::query(
            "UPDATE totp_credentials SET secret_enc = $1, key_id = $2 WHERE user_id = $3::uuid",
        )
        .bind(&s.ciphertext)
        .bind(&s.key_id)
        .bind(&id)
        .execute(&pool)
        .await
        .unwrap();
    }
    let report = after.svc.rotate_keys(2).await.unwrap();
    assert_eq!(report.reencrypted, 4);
    assert!(report.failed.is_empty());
    assert_eq!(
        report.retired,
        vec![before.svc.envelope.current_id().to_owned()]
    );
    // Idempotent.
    let again = after.svc.rotate_keys(2).await.unwrap();
    assert_eq!(again.reencrypted, 0);
    assert!(again.retired.is_empty());
    // Everything is readable with the new key alone.
    for (id, ct, key) in rows(&pool).await {
        assert_eq!(key, new.id());
        let plain = only_new
            .svc
            .open(&ct, &key, "totp_credentials", "secret_enc", &id)
            .unwrap();
        assert!(plain.expose().starts_with(b"seed-"));
    }
    let keys = slath_db::secrets::list_keys(&pool).await.unwrap();
    assert_eq!(keys.len(), 2);
    assert!(
        keys.iter()
            .any(|k| k.key_id == old_id(&keys, new.id()) && k.retired_at.is_some())
    );
}

fn old_id(keys: &[slath_db::secrets::KeyInfo], new: &str) -> String {
    keys.iter()
        .find(|k| k.key_id != new)
        .unwrap()
        .key_id
        .clone()
}
