-- SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- The readiness snapshot also keeps what a list row shows besides the
-- headline: its check state and its approval state, each a tone and words.
-- built_format says which of these a snapshot has; one of an older format
-- isn't current, so existing snapshots are computed again when a list
-- shows them, without a backfill.

ALTER TABLE mr_readiness
    ADD COLUMN built_format smallint NOT NULL DEFAULT 1,
    ADD COLUMN checks_tone text NULL CHECK (checks_tone IN ('done', 'waiting', 'blocking')),
    ADD COLUMN checks_text text NULL CHECK (length(checks_text) <= 300),
    ADD COLUMN approvals_tone text NULL CHECK (approvals_tone IN ('done', 'waiting', 'blocking')),
    ADD COLUMN approvals_text text NULL CHECK (length(approvals_text) <= 300);

UPDATE schema_info SET version = 145;
