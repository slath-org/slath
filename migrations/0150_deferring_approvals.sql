-- SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- Approvals that leave the decision to someone else: they never count
-- toward requirements, and name the user or team asked to review instead.
-- `deferred` stays set when the named user or team is deleted, so such an
-- approval never starts counting.

ALTER TABLE mr_reviews
    ADD COLUMN deferred bool NOT NULL DEFAULT false,
    ADD COLUMN defer_user_id uuid NULL REFERENCES users (id) ON DELETE SET NULL,
    ADD COLUMN defer_team_id uuid NULL REFERENCES teams (id) ON DELETE SET NULL,
    ADD CONSTRAINT mr_reviews_deferred_check CHECK (
        (NOT deferred AND defer_user_id IS NULL AND defer_team_id IS NULL)
        OR (deferred AND verdict = 'approve'
            AND (defer_user_id IS NULL OR defer_team_id IS NULL)));

UPDATE schema_info SET version = 150;
