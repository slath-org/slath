-- SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- Bots and their owners: a user's bot can do no more than its owner on a
-- repository, a bot's owner counts as owner of the bot's namespace, and
-- suspending a person suspends their bots.

-- Bots suspended because their owner was; reactivating the owner
-- reactivates exactly these.
ALTER TABLE users ADD COLUMN suspended_with_owner bool NOT NULL DEFAULT false;

-- True if `viewer` owns the namespace: it's theirs, or it belongs to a bot
-- they own (a user's bot) or whose organization they own.
CREATE FUNCTION slath_owns_namespace(ns uuid, viewer uuid) RETURNS bool
LANGUAGE sql STABLE AS $$
    SELECT EXISTS (
        SELECT 1 FROM users u WHERE u.namespace_id = ns AND (
            u.id = viewer
            OR (u.kind = 'bot' AND u.bot_owner_kind = 'user' AND u.bot_owner_id = viewer)
            OR (u.kind = 'bot' AND u.bot_owner_kind = 'org' AND EXISTS (
                SELECT 1 FROM org_members m
                WHERE m.org_id = u.bot_owner_id AND m.user_id = viewer AND m.role = 'owner'))
        )
    )
$$;

-- A user's own grants on a repository, without the bot cap.
CREATE FUNCTION slath_can_read_direct(repo uuid, viewer uuid) RETURNS bool
LANGUAGE sql STABLE AS $$
    SELECT viewer IS NOT NULL AND EXISTS (
        SELECT 1 FROM repositories r
        LEFT JOIN organizations o ON o.namespace_id = r.namespace_id
        WHERE r.id = repo
          AND NOT (COALESCE(o.require_mfa, false) AND NOT slath_has_mfa(viewer))
          AND (
               EXISTS (SELECT 1 FROM repo_collaborators c WHERE c.repo_id = r.id AND c.user_id = viewer)
            OR slath_owns_namespace(r.namespace_id, viewer)
            OR EXISTS (SELECT 1 FROM org_members m WHERE m.org_id = o.id AND m.user_id = viewer
                         AND (m.role = 'owner' OR o.default_member_role <> 'none'))
            OR EXISTS (SELECT 1 FROM repo_team_access a JOIN team_members tm ON tm.team_id = a.team_id
                       WHERE a.repo_id = r.id AND tm.user_id = viewer)
          )
    )
$$;

-- A user's bot reads a repository through its grants only if its owner can
-- too (site administrators can read everything).
CREATE OR REPLACE FUNCTION slath_can_read(repo uuid, viewer uuid) RETURNS bool
LANGUAGE sql STABLE AS $$
    SELECT slath_can_read_direct(repo, viewer) AND NOT EXISTS (
        SELECT 1 FROM users b JOIN users o ON o.id = b.bot_owner_id
        WHERE b.id = viewer AND b.kind = 'bot' AND b.bot_owner_kind = 'user'
          AND NOT (o.state = 'active' AND (o.is_admin OR slath_can_read_direct(repo, o.id)))
    )
$$;

UPDATE schema_info SET version = 127;
