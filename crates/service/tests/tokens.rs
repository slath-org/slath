// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Token checks that happen before the database is involved.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use slath_core::credential::{CredentialKind, assemble};

#[tokio::test]
async fn bad_checksums_are_rejected_without_a_database_lookup() {
    // A pool that can never connect: any lookup would be an error.
    let pool = sqlx::postgres::PgPoolOptions::new()
        .acquire_timeout(std::time::Duration::from_millis(200))
        .connect_lazy("postgres://nobody@127.0.0.1:1/none")
        .unwrap();
    let env = common::services(pool, "");
    let good = assemble(
        CredentialKind::PersonalAccessToken,
        "0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefg",
    )
    .unwrap();
    let mut bad = good.clone().into_bytes();
    bad[12] = b'z';
    let bad = String::from_utf8(bad).unwrap();
    let runner = assemble(
        CredentialKind::RunnerToken,
        "0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefg",
    )
    .unwrap();
    for t in [bad.as_str(), runner.as_str(), "slath_pat_short", ""] {
        assert!(
            env.svc.authenticate_token(t).await.unwrap().is_none(),
            "{t}"
        );
    }
    // A well-formed token does reach the database (and fails here).
    assert!(env.svc.authenticate_token(&good).await.is_err());
}
