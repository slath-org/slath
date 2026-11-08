-- SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- Changes of merge requests: each per-change revision adds one version per
-- commit, bound to a change. Aliases (jj change IDs and Gerrit Change-Id
-- trailers) name changes within one merge request only.

UPDATE schema_info SET version = 106;

CREATE TABLE mr_changes (
    id uuid PRIMARY KEY,
    mr_id uuid NOT NULL REFERENCES merge_requests (id) ON DELETE CASCADE,
    -- In jj's format: the change-id of the first version, or the ID jj
    -- gives a commit without one.
    display_id text NOT NULL CHECK (display_id ~ '^[k-z]{32}$'),
    state text NOT NULL DEFAULT 'active' CHECK (state IN ('active', 'removed', 'merged')),
    created_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (mr_id, display_id)
);
CREATE INDEX mr_changes_mr ON mr_changes (mr_id);

CREATE TABLE mr_change_aliases (
    mr_id uuid NOT NULL REFERENCES merge_requests (id) ON DELETE CASCADE,
    kind text NOT NULL CHECK (kind IN ('jj', 'gerrit')),
    value text NOT NULL CHECK (value ~ '^([k-z]{32}|I[0-9a-f]{40})$'),
    change_id uuid NOT NULL REFERENCES mr_changes (id) ON DELETE CASCADE,
    -- The revision and pusher (or resolver) that bound it.
    revision_seq int NOT NULL,
    bound_by uuid NULL REFERENCES users (id) ON DELETE SET NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (mr_id, kind, value)
);
CREATE INDEX mr_change_aliases_change ON mr_change_aliases (change_id);

CREATE TABLE mr_change_versions (
    id uuid PRIMARY KEY,
    revision_id uuid NOT NULL REFERENCES mr_revisions (id) ON DELETE CASCADE,
    -- NULL while unresolved.
    change_id uuid NULL REFERENCES mr_changes (id) ON DELETE CASCADE,
    -- 0 is the bottom of the stack.
    position int NOT NULL CHECK (position >= 0),
    commit_oid text NOT NULL CHECK (commit_oid ~ '^[0-9a-f]{40}$'),
    parent_oid text NULL CHECK (parent_oid ~ '^[0-9a-f]{40}$'),
    patch_id text NULL CHECK (patch_id ~ '^[0-9a-f]{40}$'),
    -- How the commit was bound: new (first version of a change),
    -- confirmed (alias or identical patch), inferred (content), or
    -- unresolved (conflicting or duplicate aliases; blocks merge).
    state text NOT NULL CHECK (state IN ('new', 'confirmed', 'inferred', 'unresolved')),
    -- alias, patch, content, aliases-conflict, duplicate-id, split, join,
    -- resolved.
    reason text NULL,
    -- Changes it may continue (ambiguous pairing) or must pick from
    -- (unresolved), best first.
    candidates uuid[] NOT NULL DEFAULT '{}',
    conflicted bool NOT NULL DEFAULT false,
    UNIQUE (revision_id, position),
    CHECK ((state = 'unresolved') = (change_id IS NULL))
);
CREATE INDEX mr_change_versions_change ON mr_change_versions (change_id);

ALTER TABLE timeline_events DROP CONSTRAINT timeline_events_kind_check;
ALTER TABLE timeline_events ADD CONSTRAINT timeline_events_kind_check
    CHECK (kind IN ('closed', 'reopened', 'labeled', 'unlabeled', 'assigned', 'unassigned',
        'milestoned', 'demilestoned', 'renamed', 'referenced',
        'revision', 'ready', 'draft', 'source_deleted', 'source_restored', 'merged',
        'review_requested', 'change_split', 'change_joined', 'change_resolved'));
