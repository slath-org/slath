-- SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- Commit statuses: reports on a commit by an authenticated reporter. The
-- latest report per (repository, commit, context) wins. Branch protection
-- can list, per required context, whose reports count.

UPDATE schema_info SET version = 109;

CREATE TABLE commit_statuses (
    id uuid PRIMARY KEY,
    repo_id uuid NOT NULL REFERENCES repositories (id) ON DELETE CASCADE,
    commit_oid text NOT NULL CHECK (commit_oid ~ '^[0-9a-f]{40}$'),
    context text NOT NULL CHECK (context ~ '^[A-Za-z0-9 ._/:-]{1,100}$'),
    state text NOT NULL CHECK (state IN ('pending', 'success', 'failure', 'error')),
    description text NULL CHECK (length(description) <= 1000),
    target_url text NULL CHECK (length(target_url) <= 2048),
    -- The reporter: a user or a bot account (native CI comes later).
    creator_kind text NOT NULL CHECK (creator_kind IN ('user', 'token', 'ci')),
    creator_id uuid NULL,
    created_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX commit_statuses_lookup ON commit_statuses (repo_id, commit_oid, context, created_at DESC);

-- Required context -> allowed reporter user IDs (an empty or missing list:
-- anyone with status.write, except the merge request's author and their
-- bots).
ALTER TABLE branch_protections ADD COLUMN status_reporters jsonb NOT NULL DEFAULT '{}';
