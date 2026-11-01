// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Release schema+data fixtures (`tests/fixtures/schema/v<version>.sql`):
//! loading them into a test database, and giving their repositories a
//! (empty) home on disk.

use std::path::{Path, PathBuf};

/// Every release fixture as `(version, path)`, oldest first.
#[must_use]
pub fn releases() -> Vec<(String, PathBuf)> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/schema");
    let mut out: Vec<(Vec<u64>, String, PathBuf)> = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter_map(|p| {
            let v = p
                .file_name()?
                .to_str()?
                .strip_prefix('v')?
                .strip_suffix(".sql")?
                .to_owned();
            let key = v.split('.').map(|n| n.parse().unwrap_or(0)).collect();
            Some((key, v, p))
        })
        .collect();
    out.sort();
    out.into_iter().map(|(_, v, p)| (v, p)).collect()
}

/// Loads a `pg_dump` plain-format file (with `COPY … FROM stdin` blocks)
/// into the database at `url`, without `psql`.
pub fn load(url: &str, path: &Path) {
    use sqlx::Connection;
    let text = std::fs::read_to_string(path).unwrap();
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            let mut c = sqlx::PgConnection::connect(url).await.unwrap();
            let mut sql = String::new();
            let mut lines = text.lines();
            while let Some(line) = lines.next() {
                if line.starts_with("\\restrict") || line.starts_with("\\unrestrict") {
                    continue;
                }
                if line.starts_with("COPY ") && line.ends_with("FROM stdin;") {
                    if !sql.trim().is_empty() {
                        sqlx::raw_sql(sqlx::AssertSqlSafe(std::mem::take(&mut sql)))
                            .execute(&mut c)
                            .await
                            .unwrap();
                    }
                    let mut data = String::new();
                    for row in lines.by_ref() {
                        if row == "\\." {
                            break;
                        }
                        data.push_str(row);
                        data.push('\n');
                    }
                    let stmt = line.trim_end_matches(';');
                    let mut copy = c.copy_in_raw(stmt).await.unwrap();
                    copy.send(data.into_bytes()).await.unwrap();
                    copy.finish().await.unwrap();
                    continue;
                }
                sql.push_str(line);
                sql.push('\n');
            }
            if !sql.trim().is_empty() {
                sqlx::raw_sql(sqlx::AssertSqlSafe(sql))
                    .execute(&mut c)
                    .await
                    .unwrap();
            }
        });
}

/// Creates an empty bare repository for every repository row (live ones
/// under `repos/`, deleted ones under `trash/`), as a restored installation
/// would have them.
pub fn create_repo_dirs(db: &crate::server::TestDb, data_dir: &Path) {
    let rows = db.query_one(
        "SELECT coalesce(string_agg(id::text || ':' || state, ','), '') FROM repositories",
    );
    for row in rows.split(',').filter(|r| !r.is_empty()) {
        let (id, state) = row.split_once(':').unwrap();
        let path = if state == "deleting" {
            data_dir.join("trash").join(format!("{id}.git"))
        } else {
            data_dir
                .join("repos")
                .join(&id[..2])
                .join(format!("{id}.git"))
        };
        std::fs::create_dir_all(&path).unwrap();
        let out = std::process::Command::new(crate::server::git_bin())
            .args(["init", "--bare", "-q"])
            .arg(&path)
            .output()
            .unwrap();
        assert!(out.status.success());
    }
}
