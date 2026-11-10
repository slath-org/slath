// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! The derived ref snapshot (`git_refs`) and push history. Git is the
//! source of truth; these tables are rebuilt by the reconciler.

use slath_core::ids::{RepoId, UserId};
use slath_git::ipc::RefUpdate;
use sqlx::PgExecutor;

use crate::DbError;

fn oid(s: &str) -> Option<&str> {
    (!s.bytes().all(|b| b == b'0')).then_some(s)
}

/// Inserts a push event with its updates and applies them to the snapshot.
/// Returns the push ID. Run in a transaction.
pub async fn record_push(
    tx: &mut sqlx::PgConnection,
    repo: RepoId,
    pusher: Option<UserId>,
    transport: &str,
    updates: &[RefUpdate],
) -> Result<uuid::Uuid, DbError> {
    let push_id = uuid::Uuid::now_v7();
    sqlx::query!(
        "INSERT INTO push_events (id, repo_id, pusher_id, transport) VALUES ($1, $2, $3, $4)",
        push_id,
        repo.as_uuid(),
        pusher.map(|p| *p.as_uuid()),
        transport
    )
    .execute(&mut *tx)
    .await?;
    for (seq, u) in updates.iter().enumerate() {
        let seq = i32::try_from(seq).map_err(|_| DbError::Invalid("too many ref updates"))?;
        sqlx::query!(
            "INSERT INTO ref_updates (push_id, seq, name, old_oid, new_oid) VALUES ($1, $2, $3, $4, $5)",
            push_id,
            seq,
            &u.name,
            oid(&u.old),
            oid(&u.new)
        )
        .execute(&mut *tx)
        .await?;
        match oid(&u.new) {
            Some(new) => {
                sqlx::query!(
                    "INSERT INTO git_refs (repo_id, name, oid) VALUES ($1, $2, $3)
                     ON CONFLICT (repo_id, name) DO UPDATE SET oid = EXCLUDED.oid, updated_at = now()",
                    repo.as_uuid(),
                    &u.name,
                    new
                )
                .execute(&mut *tx)
                .await?;
            }
            None => {
                sqlx::query!(
                    "DELETE FROM git_refs WHERE repo_id = $1 AND name = $2",
                    repo.as_uuid(),
                    &u.name
                )
                .execute(&mut *tx)
                .await?;
            }
        }
    }
    sqlx::query!(
        "UPDATE repositories SET pushed_at = now() WHERE id = $1",
        repo.as_uuid()
    )
    .execute(&mut *tx)
    .await?;
    Ok(push_id)
}

/// The snapshot of a repository's refs as `(name, oid)`.
pub async fn snapshot(
    exec: impl PgExecutor<'_>,
    repo: RepoId,
) -> Result<Vec<(Vec<u8>, String)>, DbError> {
    let rows = sqlx::query!(
        "SELECT name, oid FROM git_refs WHERE repo_id = $1 ORDER BY name",
        repo.as_uuid()
    )
    .fetch_all(exec)
    .await?;
    Ok(rows.into_iter().map(|r| (r.name, r.oid)).collect())
}

/// Pushes recorded for a repository (newest first).
pub async fn push_count(
    exec: impl PgExecutor<'_>,
    repo: RepoId,
    transport: Option<&str>,
) -> Result<i64, DbError> {
    Ok(sqlx::query_scalar!(
        r#"SELECT count(*) AS "n!" FROM push_events WHERE repo_id = $1 AND ($2::text IS NULL OR transport = $2)"#,
        repo.as_uuid(),
        transport
    )
    .fetch_one(exec)
    .await?)
}

/// The pusher and ref updates of one recorded push.
pub async fn push_updates(
    exec: impl PgExecutor<'_> + Copy,
    push: uuid::Uuid,
) -> Result<Option<(Option<UserId>, Vec<RefUpdate>)>, DbError> {
    let Some(pusher) = sqlx::query_scalar!("SELECT pusher_id FROM push_events WHERE id = $1", push)
        .fetch_optional(exec)
        .await?
    else {
        return Ok(None);
    };
    const ZERO: &str = "0000000000000000000000000000000000000000";
    let rows = sqlx::query!(
        "SELECT name, old_oid, new_oid FROM ref_updates WHERE push_id = $1 ORDER BY seq",
        push
    )
    .fetch_all(exec)
    .await?;
    Ok(Some((
        pusher.map(UserId::from_uuid),
        rows.into_iter()
            .map(|r| RefUpdate {
                name: r.name,
                old: r.old_oid.unwrap_or_else(|| ZERO.to_owned()),
                new: r.new_oid.unwrap_or_else(|| ZERO.to_owned()),
            })
            .collect(),
    )))
}

/// Branches `pusher` pushed to `repo` since `since`, newest first.
pub async fn recent_branch_pushes(
    exec: impl PgExecutor<'_>,
    repo: RepoId,
    pusher: UserId,
    since: time::OffsetDateTime,
) -> Result<Vec<Vec<u8>>, DbError> {
    let rows = sqlx::query!(
        r#"SELECT u.name AS "name!", max(p.created_at) AS "at!"
           FROM push_events p JOIN ref_updates u ON u.push_id = p.id
           WHERE p.repo_id = $1 AND p.pusher_id = $2 AND p.created_at > $3
             AND u.new_oid IS NOT NULL AND u.name LIKE 'refs/heads/%'
           GROUP BY u.name ORDER BY 2 DESC LIMIT 20"#,
        repo.as_uuid(),
        pusher.as_uuid(),
        since
    )
    .fetch_all(exec)
    .await?;
    Ok(rows.into_iter().map(|r| r.name).collect())
}

/// How a push arrived: `http`, `ssh`, `internal`, or `reconcile`.
pub async fn push_transport(
    exec: impl PgExecutor<'_>,
    push: uuid::Uuid,
) -> Result<Option<String>, DbError> {
    Ok(
        sqlx::query_scalar!("SELECT transport FROM push_events WHERE id = $1", push)
            .fetch_optional(exec)
            .await?,
    )
}
