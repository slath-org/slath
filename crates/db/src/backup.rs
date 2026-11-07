// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Logical database backup and restore without external tools.
//!
//! A dump is one `COPY … TO STDOUT` (text format) per table, all read in a
//! single `REPEATABLE READ` snapshot, plus the sequence positions. A restore
//! migrates an empty database to the dump's schema version, loads the tables
//! in foreign-key order, sets the sequences, and then applies the remaining
//! migrations. Table and column names from a dump are only used after they
//! were matched against the target database's catalog.

use std::collections::{BTreeMap, BTreeSet};

use futures_util::TryStreamExt;
use serde::{Deserialize, Serialize};
use sqlx::{AssertSqlSafe, PgConnection, PgPool};

use crate::{DbError, MIGRATOR};

/// Tables that migrations own; they're never dumped or loaded.
const MIGRATION_TABLES: &[&str] = &["_sqlx_migrations", "schema_info"];

/// One dumped table.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TableInfo {
    pub name: String,
    pub columns: Vec<String>,
    pub rows: u64,
}

/// A sequence's position.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SequenceInfo {
    pub name: String,
    pub last_value: i64,
    pub is_called: bool,
}

/// What a dump contains, apart from the table data.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DumpInfo {
    pub schema_version: i32,
    pub server_version: String,
    pub tables: Vec<TableInfo>,
    pub sequences: Vec<SequenceInfo>,
}

fn quote(ident: &str) -> String {
    format!("\"{}\"", ident.replace('"', "\"\""))
}

/// Base tables of the `public` schema in an order where every table comes
/// after the tables it references.
async fn tables_in_order(conn: &mut PgConnection) -> Result<Vec<String>, DbError> {
    let tables: Vec<String> = sqlx::query_scalar(
        "SELECT c.relname::text FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace
         WHERE n.nspname = 'public' AND c.relkind IN ('r', 'p') ORDER BY c.relname",
    )
    .fetch_all(&mut *conn)
    .await?;
    let edges: Vec<(String, String)> = sqlx::query_as(
        "SELECT c.relname::text, r.relname::text FROM pg_constraint k
         JOIN pg_class c ON c.oid = k.conrelid JOIN pg_class r ON r.oid = k.confrelid
         JOIN pg_namespace n ON n.oid = c.relnamespace
         WHERE k.contype = 'f' AND n.nspname = 'public' AND c.oid <> r.oid",
    )
    .fetch_all(&mut *conn)
    .await?;
    let mut deps: BTreeMap<&str, BTreeSet<&str>> = tables
        .iter()
        .map(|t| (t.as_str(), BTreeSet::new()))
        .collect();
    for (from, to) in &edges {
        if let Some(d) = deps.get_mut(from.as_str()) {
            d.insert(to.as_str());
        }
    }
    let mut out: Vec<String> = Vec::with_capacity(tables.len());
    while !deps.is_empty() {
        let ready: Vec<&str> = deps
            .iter()
            .filter(|(_, d)| {
                d.iter()
                    .all(|x| out.iter().any(|o| o == x) || !deps.contains_key(x))
            })
            .map(|(t, _)| *t)
            .collect();
        if ready.is_empty() {
            return Err(DbError::Backup(
                "foreign keys form a cycle between tables".into(),
            ));
        }
        for t in ready {
            deps.remove(t);
            out.push(t.to_owned());
        }
    }
    Ok(out
        .into_iter()
        .filter(|t| !MIGRATION_TABLES.contains(&t.as_str()))
        .collect())
}

async fn columns(conn: &mut PgConnection, table: &str) -> Result<Vec<String>, DbError> {
    Ok(sqlx::query_scalar(
        "SELECT a.attname::text FROM pg_attribute a
         JOIN pg_class c ON c.oid = a.attrelid JOIN pg_namespace n ON n.oid = c.relnamespace
         WHERE n.nspname = 'public' AND c.relname = $1 AND a.attnum > 0
           AND NOT a.attisdropped AND a.attgenerated = ''
         ORDER BY a.attnum",
    )
    .bind(table)
    .fetch_all(&mut *conn)
    .await?)
}

async fn sequence_names(conn: &mut PgConnection) -> Result<Vec<String>, DbError> {
    Ok(sqlx::query_scalar(
        "SELECT c.relname::text FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace
         WHERE n.nspname = 'public' AND c.relkind = 'S' ORDER BY c.relname",
    )
    .fetch_all(&mut *conn)
    .await?)
}

/// Dumps every table in one consistent snapshot. `sink` receives each
/// table's name and its `COPY` text data in chunks (an empty chunk last).
pub async fn dump<S>(pool: &PgPool, mut sink: S) -> Result<DumpInfo, DbError>
where
    S: FnMut(&str, &[u8]) -> std::io::Result<()>,
{
    let mut tx = pool.begin().await?;
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ, READ ONLY")
        .execute(&mut *tx)
        .await?;
    let schema_version: i32 = sqlx::query_scalar("SELECT version FROM schema_info")
        .fetch_one(&mut *tx)
        .await?;
    let server_version: String = sqlx::query_scalar("SHOW server_version")
        .fetch_one(&mut *tx)
        .await?;
    let mut tables = Vec::new();
    for name in tables_in_order(&mut tx).await? {
        let cols = columns(&mut tx, &name).await?;
        let list: Vec<String> = cols.iter().map(|c| quote(c)).collect();
        let stmt = format!(
            "COPY public.{} ({}) TO STDOUT",
            quote(&name),
            list.join(", ")
        );
        let mut rows = 0u64;
        {
            let mut stream = tx.copy_out_raw(&stmt).await?;
            while let Some(chunk) = stream.try_next().await? {
                rows += chunk.iter().filter(|b| **b == b'\n').count() as u64;
                sink(&name, &chunk).map_err(|e| DbError::Backup(e.to_string()))?;
            }
        }
        // Tables with no rows still get (empty) data.
        sink(&name, &[]).map_err(|e| DbError::Backup(e.to_string()))?;
        tables.push(TableInfo {
            name,
            columns: cols,
            rows,
        });
    }
    let mut sequences = Vec::new();
    for name in sequence_names(&mut tx).await? {
        let (last_value, is_called): (i64, bool) = sqlx::query_as(AssertSqlSafe(format!(
            "SELECT last_value, is_called FROM public.{}",
            quote(&name)
        )))
        .fetch_one(&mut *tx)
        .await?;
        sequences.push(SequenceInfo {
            name,
            last_value,
            is_called,
        });
    }
    tx.commit().await?;
    Ok(DumpInfo {
        schema_version,
        server_version,
        tables,
        sequences,
    })
}

/// True if the database has no tables at all (a restore target).
pub async fn is_empty(pool: &PgPool) -> Result<bool, DbError> {
    let n: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace
         WHERE n.nspname NOT IN ('pg_catalog', 'information_schema', 'pg_toast')
           AND c.relkind IN ('r', 'p', 'v', 'm', 'S')",
    )
    .fetch_one(pool)
    .await?;
    Ok(n == 0)
}

/// Loads a dump into an empty database and migrates it to the current
/// schema. `source` returns the data of a table (by name) as an async reader.
pub async fn restore<R, F, Fut>(
    pool: &PgPool,
    info: &DumpInfo,
    mut source: F,
) -> Result<(), DbError>
where
    F: FnMut(&str) -> Fut,
    Fut: std::future::Future<Output = std::io::Result<R>>,
    R: tokio::io::AsyncRead + Unpin,
{
    if !is_empty(pool).await? {
        return Err(DbError::Backup("the target database is not empty".into()));
    }
    let version = i64::from(info.schema_version);
    if crate::is_preview_schema(info.schema_version) {
        return Err(DbError::Backup(format!(
            "the backup has schema version {}, from a preview release of Slath. {}",
            info.schema_version,
            crate::PREVIEW_GUIDANCE
        )));
    }
    if info.schema_version < crate::BASELINE_SCHEMA || info.schema_version > crate::REQUIRED_SCHEMA
    {
        return Err(DbError::Backup(format!(
            "the backup has schema version {}, this binary supports up to {}",
            info.schema_version,
            crate::REQUIRED_SCHEMA
        )));
    }
    MIGRATOR.run_to(version, pool).await?;
    let mut tx = pool.begin().await?;
    let expected = tables_in_order(&mut tx).await?;
    let got: Vec<&str> = info.tables.iter().map(|t| t.name.as_str()).collect();
    let mut want_sorted: Vec<&str> = expected.iter().map(String::as_str).collect();
    let mut got_sorted = got.clone();
    want_sorted.sort_unstable();
    got_sorted.sort_unstable();
    if want_sorted != got_sorted {
        return Err(DbError::Backup(
            "the backup's tables don't match its schema version".into(),
        ));
    }
    // Seed rows written by migrations are replaced by the backup's.
    let all: Vec<String> = expected
        .iter()
        .map(|t| format!("public.{}", quote(t)))
        .collect();
    if !all.is_empty() {
        sqlx::query(AssertSqlSafe(format!(
            "TRUNCATE {} CASCADE",
            all.join(", ")
        )))
        .execute(&mut *tx)
        .await?;
    }
    for name in &expected {
        let Some(t) = info.tables.iter().find(|t| &t.name == name) else {
            continue;
        };
        let cols = columns(&mut tx, name).await?;
        if cols != t.columns {
            return Err(DbError::Backup(format!(
                "the columns of table {name} don't match"
            )));
        }
        let list: Vec<String> = cols.iter().map(|c| quote(c)).collect();
        let stmt = format!(
            "COPY public.{} ({}) FROM STDIN",
            quote(name),
            list.join(", ")
        );
        let reader = source(name)
            .await
            .map_err(|e| DbError::Backup(e.to_string()))?;
        let mut copy = tx.copy_in_raw(&stmt).await?;
        copy.read_from(reader).await?;
        let rows = copy.finish().await?;
        if rows != t.rows {
            return Err(DbError::Backup(format!(
                "table {name}: loaded {rows} rows, the backup lists {}",
                t.rows
            )));
        }
    }
    // Rows were loaded with their triggers active; derived counters are
    // recomputed from the loaded data.
    if expected.iter().any(|t| t == "blobs") {
        sqlx::query("SELECT slath_blob_recount()")
            .execute(&mut *tx)
            .await?;
    }
    let sequences = sequence_names(&mut tx).await?;
    for s in &info.sequences {
        if !sequences.contains(&s.name) {
            return Err(DbError::Backup(format!("unknown sequence {}", s.name)));
        }
        sqlx::query("SELECT setval($1::regclass, $2, $3)")
            .bind(format!("public.{}", quote(&s.name)))
            .bind(s.last_value)
            .bind(s.is_called)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    MIGRATOR.run(pool).await?;
    Ok(())
}
