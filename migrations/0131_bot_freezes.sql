-- SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- Freezes: while one applies, every bot of the instance (org_id NULL) or
-- of one organization is refused, without revoking anything.

CREATE TABLE bot_freezes (
    org_id uuid NULL REFERENCES organizations (id) ON DELETE CASCADE,
    frozen_by uuid NULL REFERENCES users (id) ON DELETE SET NULL,
    frozen_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE NULLS NOT DISTINCT (org_id)
);

-- True if a freeze applies to the bot `bot` (false for people).
CREATE FUNCTION slath_bot_frozen(bot uuid) RETURNS bool
LANGUAGE sql STABLE AS $$
    SELECT EXISTS (
        SELECT 1 FROM users u JOIN bot_freezes f
            ON f.org_id IS NULL OR (u.bot_owner_kind = 'org' AND f.org_id = u.bot_owner_id)
        WHERE u.id = bot AND u.kind = 'bot'
    )
$$;

UPDATE schema_info SET version = 131;
