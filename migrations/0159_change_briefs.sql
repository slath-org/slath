-- SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- Change briefs: what the author of a change version says about it (what
-- it's for, how it does it, how it was checked, what's missing). A brief
-- belongs to a change and one of its versions' commits; writing again adds
-- an edit, and a version's brief is its newest edit. Rows are never
-- changed. `schema` leaves room for structured briefs later.

CREATE TABLE change_briefs (
    id uuid PRIMARY KEY,
    mr_id uuid NOT NULL REFERENCES merge_requests (id) ON DELETE CASCADE,
    change_id uuid NOT NULL REFERENCES mr_changes (id) ON DELETE CASCADE,
    commit_oid text NOT NULL CHECK (commit_oid ~ '^[0-9a-f]{40}$'),
    author_id uuid NULL REFERENCES users (id) ON DELETE SET NULL,
    origin text NOT NULL
        CHECK (origin IN ('writer', 'member', 'bot', 'outside_contributor', 'anonymous')),
    schema text NOT NULL CHECK (schema = 'slath-brief/1'),
    intent text NOT NULL CHECK (length(intent) BETWEEN 1 AND 16384),
    approach text NULL CHECK (length(approach) BETWEEN 1 AND 16384),
    verification text NULL CHECK (length(verification) BETWEEN 1 AND 16384),
    gaps text NULL CHECK (length(gaps) BETWEEN 1 AND 16384),
    created_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX change_briefs_change ON change_briefs (change_id, created_at DESC, id DESC);
CREATE INDEX change_briefs_mr ON change_briefs (mr_id);

UPDATE schema_info SET version = 159;
