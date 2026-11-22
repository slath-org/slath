-- SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- Hunks a reviewer marked viewed, by path and the hunk's key (the SHA-256
-- of its changed lines), so a hunk that a new revision leaves unchanged
-- stays viewed. Private to the reviewer, like viewed files.

CREATE TABLE mr_viewed_hunks (
    mr_id uuid NOT NULL REFERENCES merge_requests (id) ON DELETE CASCADE,
    user_id uuid NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    path text NOT NULL,
    hunk_key text NOT NULL CHECK (hunk_key ~ '^[0-9a-f]{64}$'),
    PRIMARY KEY (mr_id, user_id, path, hunk_key)
);

UPDATE schema_info SET version = 151;
