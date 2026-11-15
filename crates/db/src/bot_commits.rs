// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! The commits that first reached a fork network in a bot's push.

use slath_core::ids::{RepoId, UserId};
use sqlx::PgExecutor;

use crate::DbError;

/// Records that `bot` brought `oids` into the network of `repo`. A commit
/// another bot brought first keeps that bot.
pub async fn insert(
    exec: impl PgExecutor<'_>,
    repo: RepoId,
    bot: UserId,
    oids: &[String],
) -> Result<(), DbError> {
    sqlx::query!(
        "INSERT INTO bot_commits (network_id, oid, bot_id)
         SELECT r.fork_network_id, o, $2 FROM repositories r, unnest($3::text[]) AS o
         WHERE r.id = $1
         ON CONFLICT DO NOTHING",
        repo.as_uuid(),
        bot.as_uuid(),
        oids
    )
    .execute(exec)
    .await?;
    Ok(())
}

/// Of `oids`, the ones a bot brought into the network of `repo`, with the
/// bot's name. Deleted bots don't count.
pub async fn bots(
    exec: impl PgExecutor<'_>,
    repo: RepoId,
    oids: &[String],
) -> Result<Vec<(String, String)>, DbError> {
    let rows = sqlx::query!(
        r#"SELECT b.oid AS "oid!", n.name::text AS "name!"
           FROM bot_commits b
           JOIN repositories r ON r.fork_network_id = b.network_id
           JOIN users u ON u.id = b.bot_id
           JOIN namespaces n ON n.id = u.namespace_id
           WHERE r.id = $1 AND b.oid = ANY($2::text[]) AND u.state <> 'deleted'"#,
        repo.as_uuid(),
        oids
    )
    .fetch_all(exec)
    .await?;
    Ok(rows.into_iter().map(|r| (r.oid, r.name)).collect())
}
