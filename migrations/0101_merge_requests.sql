-- SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- Merge requests, their reviewers, assignees, and labels, and their
-- revisions (one per push of the source). Numbers come from
-- repositories.next_item_number, shared with issues. Each revision's head
-- is pinned by the ref refs/slath/mr/<id>/r<seq>, so the commits stay
-- readable after force pushes and garbage collection.

UPDATE schema_info SET version = 101;

CREATE TABLE merge_requests (
    id uuid PRIMARY KEY,
    repo_id uuid NOT NULL REFERENCES repositories (id) ON DELETE CASCADE,
    number int NOT NULL CHECK (number > 0),
    author_id uuid NULL REFERENCES users (id) ON DELETE SET NULL,
    title text NOT NULL CHECK (length(title) BETWEEN 1 AND 256),
    description text NOT NULL DEFAULT '',
    draft bool NOT NULL DEFAULT false,
    -- Short branch name in repo_id.
    target_branch text NOT NULL CHECK (length(target_branch) BETWEEN 1 AND 255),
    -- repo_id itself or a fork in its network; NULL once that fork is deleted.
    source_repo_id uuid NULL REFERENCES repositories (id) ON DELETE SET NULL,
    -- Full ref name in the source repository (refs/heads/...).
    source_ref bytea NOT NULL,
    allow_maintainer_edits bool NOT NULL DEFAULT true,
    state text NOT NULL DEFAULT 'open' CHECK (state IN ('open', 'merged', 'closed')),
    milestone_id uuid NULL REFERENCES milestones (id) ON DELETE SET NULL,
    -- "Merge when ready".
    auto_merge_strategy text NULL
        CHECK (auto_merge_strategy IN ('fast-forward', 'rebase', 'merge-commit', 'squash')),
    auto_merge_by uuid NULL REFERENCES users (id) ON DELETE SET NULL,
    auto_merge_set_at timestamptz NULL,
    delete_source_on_merge bool NOT NULL DEFAULT true,
    merged_by uuid NULL REFERENCES users (id) ON DELETE SET NULL,
    merged_at timestamptz NULL,
    merge_strategy text NULL
        CHECK (merge_strategy IN ('fast-forward', 'rebase', 'merge-commit', 'squash')),
    merged_oid text NULL CHECK (merged_oid ~ '^[0-9a-f]{40}$'),
    closed_at timestamptz NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    search tsvector GENERATED ALWAYS AS (
        setweight(to_tsvector('simple', title), 'A') || setweight(to_tsvector('simple', description), 'B')
    ) STORED,
    UNIQUE (repo_id, number),
    CHECK ((state = 'merged') = (merged_at IS NOT NULL))
);
-- At most one open merge request per source and target.
CREATE UNIQUE INDEX merge_requests_open_pair
    ON merge_requests (source_repo_id, source_ref, repo_id, target_branch) WHERE state = 'open';
CREATE INDEX merge_requests_repo_state ON merge_requests (repo_id, state, updated_at DESC);
CREATE INDEX merge_requests_source ON merge_requests (source_repo_id, source_ref) WHERE state = 'open';
CREATE INDEX merge_requests_author ON merge_requests (author_id);
CREATE INDEX merge_requests_search ON merge_requests USING gin (search);

-- Requested reviewers: a user or a team.
CREATE TABLE mr_reviewers (
    id uuid PRIMARY KEY,
    mr_id uuid NOT NULL REFERENCES merge_requests (id) ON DELETE CASCADE,
    user_id uuid NULL REFERENCES users (id) ON DELETE CASCADE,
    team_id uuid NULL REFERENCES teams (id) ON DELETE CASCADE,
    requested_by uuid NULL REFERENCES users (id) ON DELETE SET NULL,
    state text NOT NULL DEFAULT 'requested' CHECK (state IN ('requested', 'reviewed', 'removed')),
    created_at timestamptz NOT NULL DEFAULT now(),
    CHECK ((user_id IS NULL) <> (team_id IS NULL))
);
CREATE UNIQUE INDEX mr_reviewers_user ON mr_reviewers (mr_id, user_id) WHERE user_id IS NOT NULL;
CREATE UNIQUE INDEX mr_reviewers_team ON mr_reviewers (mr_id, team_id) WHERE team_id IS NOT NULL;
CREATE INDEX mr_reviewers_requested ON mr_reviewers (user_id) WHERE state = 'requested';

CREATE TABLE mr_assignees (
    mr_id uuid NOT NULL REFERENCES merge_requests (id) ON DELETE CASCADE,
    user_id uuid NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    PRIMARY KEY (mr_id, user_id)
);
CREATE INDEX mr_assignees_user ON mr_assignees (user_id);

CREATE TABLE mr_labels (
    mr_id uuid NOT NULL REFERENCES merge_requests (id) ON DELETE CASCADE,
    label_id uuid NOT NULL REFERENCES labels (id) ON DELETE CASCADE,
    PRIMARY KEY (mr_id, label_id)
);
CREATE INDEX mr_labels_label ON mr_labels (label_id);

-- One row per push of the source that changed its head. The current
-- revision is the one with the highest seq.
CREATE TABLE mr_revisions (
    id uuid PRIMARY KEY,
    mr_id uuid NOT NULL REFERENCES merge_requests (id) ON DELETE CASCADE,
    seq int NOT NULL CHECK (seq > 0),
    -- Target branch head when the revision was recorded.
    base_oid text NOT NULL CHECK (base_oid ~ '^[0-9a-f]{40}$'),
    merge_base_oid text NULL CHECK (merge_base_oid ~ '^[0-9a-f]{40}$'),
    head_oid text NOT NULL CHECK (head_oid ~ '^[0-9a-f]{40}$'),
    pushed_by uuid NULL REFERENCES users (id) ON DELETE SET NULL,
    commit_count int NOT NULL DEFAULT 0 CHECK (commit_count >= 0),
    mode text NOT NULL DEFAULT 'per-change' CHECK (mode IN ('per-change', 'whole-only')),
    -- Why a revision is reviewed only as a whole, e.g. 'merge-commits'.
    mode_reason text NULL,
    -- Set by the reconciler when the pinned head is gone from Git.
    missing_objects bool NOT NULL DEFAULT false,
    created_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (mr_id, seq)
);
