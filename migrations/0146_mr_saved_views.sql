-- SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- A person's saved views of a repository's merge request list: a name and
-- the list's query string (state, filters, view, order), never results.
-- Opening one shows what its owner can read at that moment.

CREATE TABLE mr_saved_views (
    id uuid PRIMARY KEY,
    user_id uuid NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    repo_id uuid NOT NULL REFERENCES repositories (id) ON DELETE CASCADE,
    name text NOT NULL CHECK (length(name) BETWEEN 1 AND 60),
    query text NOT NULL CHECK (length(query) BETWEEN 1 AND 1000),
    created_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (user_id, repo_id, name)
);
CREATE INDEX mr_saved_views_repo ON mr_saved_views (repo_id);

UPDATE schema_info SET version = 146;
