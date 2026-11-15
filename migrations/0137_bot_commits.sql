-- SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- The commits that first reached a fork network in a bot's push. Pages
-- show the bot as their committer, and as their author unless a person's
-- confirmed address is written there. Recorded when the push's checks
-- pass, from the objects it brought (still in quarantine); the first bot
-- to bring a commit keeps it.

CREATE TABLE bot_commits (
    network_id uuid NOT NULL,
    oid text NOT NULL CHECK (oid ~ '^[0-9a-f]{40}$'),
    bot_id uuid NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    created_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (network_id, oid)
);

UPDATE schema_info SET version = 137;
