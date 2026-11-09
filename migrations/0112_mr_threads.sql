-- SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- Review threads of merge requests, anchored to lines of an immutable
-- diff (two commits, a path, a side, a line range, and the hash of those
-- lines), with comments that stay pending until their review is submitted.

UPDATE schema_info SET version = 112;

CREATE TABLE mr_threads (
    id uuid PRIMARY KEY,
    mr_id uuid NOT NULL REFERENCES merge_requests (id) ON DELETE CASCADE,
    revision_seq int NOT NULL CHECK (revision_seq > 0),
    -- The diff the thread was made on: old_oid..new_oid. NULL for threads
    -- on the merge request as a whole.
    old_oid text NULL CHECK (old_oid ~ '^[0-9a-f]{40}$'),
    new_oid text NULL CHECK (new_oid ~ '^[0-9a-f]{40}$'),
    path text NULL CHECK (length(path) BETWEEN 1 AND 4096),
    side text NULL CHECK (side IN ('old', 'new')),
    line_start int NULL CHECK (line_start > 0),
    line_end int NULL,
    -- SHA-256 of the anchored lines, and up to 3 lines around them.
    line_hash text NULL CHECK (line_hash ~ '^[0-9a-f]{64}$'),
    context jsonb NOT NULL DEFAULT '{}',
    author_id uuid NULL REFERENCES users (id) ON DELETE SET NULL,
    resolved_at timestamptz NULL,
    resolved_by uuid NULL REFERENCES users (id) ON DELETE SET NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    CHECK ((path IS NULL) = (side IS NULL) AND (path IS NULL) = (line_start IS NULL)),
    CHECK (line_end IS NULL OR line_end >= line_start)
);
CREATE INDEX mr_threads_mr ON mr_threads (mr_id, created_at);

CREATE TABLE mr_thread_comments (
    id uuid PRIMARY KEY,
    thread_id uuid NOT NULL REFERENCES mr_threads (id) ON DELETE CASCADE,
    author_id uuid NULL REFERENCES users (id) ON DELETE SET NULL,
    body text NOT NULL,
    -- Pending comments belong to their author's unsubmitted review.
    pending bool NOT NULL DEFAULT false,
    created_at timestamptz NOT NULL DEFAULT now(),
    edited_at timestamptz NULL,
    deleted_at timestamptz NULL
);
CREATE INDEX mr_thread_comments_thread ON mr_thread_comments (thread_id, created_at);
CREATE INDEX mr_thread_comments_pending ON mr_thread_comments (author_id) WHERE pending;

-- Files a reviewer marked viewed, with the blob they saw.
CREATE TABLE mr_viewed_files (
    mr_id uuid NOT NULL REFERENCES merge_requests (id) ON DELETE CASCADE,
    user_id uuid NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    path text NOT NULL,
    blob_oid text NOT NULL CHECK (blob_oid ~ '^[0-9a-f]{40}$'),
    PRIMARY KEY (mr_id, user_id, path)
);

ALTER TABLE timeline_events DROP CONSTRAINT timeline_events_kind_check;
ALTER TABLE timeline_events ADD CONSTRAINT timeline_events_kind_check
    CHECK (kind IN ('closed', 'reopened', 'labeled', 'unlabeled', 'assigned', 'unassigned',
        'milestoned', 'demilestoned', 'renamed', 'referenced',
        'revision', 'ready', 'draft', 'source_deleted', 'source_restored', 'merged',
        'review_requested', 'review_request_removed', 'change_split', 'change_joined',
        'change_resolved', 'approvals_carried', 'approvals_stale', 'thread_resolved',
        'thread_reopened'));
