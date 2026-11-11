-- SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- Who chose "Check out" on a fork merge request: its branch mr/<n> shows
-- in their fetches of the target, next to its reviewers and assignees.

CREATE TABLE mr_checkouts (
    mr_id uuid NOT NULL REFERENCES merge_requests (id) ON DELETE CASCADE,
    user_id uuid NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    created_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (mr_id, user_id)
);
CREATE INDEX mr_checkouts_user ON mr_checkouts (user_id);

UPDATE schema_info SET version = 122;
