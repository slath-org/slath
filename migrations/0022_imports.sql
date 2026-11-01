-- SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- Repository imports from other servers: one row per imported repository
-- with its progress and the report shown afterwards. The URL is stored
-- without credentials.

UPDATE schema_info SET version = 22;

CREATE TABLE repo_imports (
    repo_id uuid PRIMARY KEY REFERENCES repositories (id) ON DELETE CASCADE,
    url text NOT NULL,
    requested_by uuid NULL REFERENCES users (id) ON DELETE SET NULL,
    state text NOT NULL DEFAULT 'queued' CHECK (state IN ('queued', 'running', 'done', 'failed')),
    error text NULL,
    branches int NULL,
    tags int NULL,
    size_bytes bigint NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    started_at timestamptz NULL,
    finished_at timestamptz NULL
);
CREATE INDEX repo_imports_unfinished ON repo_imports (state) WHERE state IN ('queued', 'running');
