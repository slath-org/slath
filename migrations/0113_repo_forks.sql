-- SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- Forks: the job that copies the parent's branches and tags into a new
-- repository, and the most a fork may ever be seen by. A fork of a private
-- repository stays private even after its parent is deleted.

CREATE TABLE repo_forks (
    repo_id uuid PRIMARY KEY REFERENCES repositories (id) ON DELETE CASCADE,
    -- The parent at the time of forking (`repositories.fork_parent_id` is
    -- the live link and becomes NULL when the parent is purged).
    parent_id uuid NULL REFERENCES repositories (id) ON DELETE SET NULL,
    default_branch_only bool NOT NULL DEFAULT false,
    max_visibility text NOT NULL CHECK (max_visibility IN ('private', 'internal', 'public')),
    state text NOT NULL DEFAULT 'queued' CHECK (state IN ('queued', 'running', 'done', 'failed')),
    error text NULL CHECK (length(error) <= 500),
    requested_by uuid NULL REFERENCES users (id) ON DELETE SET NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    finished_at timestamptz NULL
);
CREATE INDEX repo_forks_parent ON repo_forks (parent_id);

UPDATE schema_info SET version = 113;
