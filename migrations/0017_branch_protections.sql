-- SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- Branch protection rules. Columns for merge requests (approvals, status
-- contexts, threads) are used from the merge request milestone on.

UPDATE schema_info SET version = 17;

CREATE TABLE branch_protections (
    id uuid PRIMARY KEY,
    repo_id uuid NOT NULL REFERENCES repositories (id) ON DELETE CASCADE,
    -- Glob on the short branch name: `*` matches within a path segment.
    pattern text NOT NULL CHECK (length(pattern) BETWEEN 1 AND 255),
    block_force_push bool NOT NULL DEFAULT true,
    block_deletion bool NOT NULL DEFAULT true,
    require_mr bool NOT NULL DEFAULT false,
    required_approvals int NOT NULL DEFAULT 1 CHECK (required_approvals BETWEEN 0 AND 20),
    required_status_contexts text[] NOT NULL DEFAULT '{}',
    push_role text NOT NULL DEFAULT 'write'
        CHECK (push_role IN ('write', 'maintain', 'admin', 'nobody')),
    dismiss_stale_approvals bool NOT NULL DEFAULT true,
    statuses_on_head_ok bool NOT NULL DEFAULT true,
    require_resolved_threads bool NOT NULL DEFAULT false,
    bots_count_as_approvers bool NOT NULL DEFAULT false,
    bot_merge_allowlist uuid[] NOT NULL DEFAULT '{}',
    require_up_to_date bool NOT NULL DEFAULT false,
    created_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (repo_id, pattern)
);
