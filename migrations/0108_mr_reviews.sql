-- SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- Reviews of merge requests: a verdict on one revision, with an optional
-- body. A reviewer's latest approval or request for changes counts; a
-- revision that keeps every change's patch the same (a pure rebase) keeps
-- earlier approvals current.

UPDATE schema_info SET version = 108;

CREATE TABLE mr_reviews (
    id uuid PRIMARY KEY,
    mr_id uuid NOT NULL REFERENCES merge_requests (id) ON DELETE CASCADE,
    revision_seq int NOT NULL CHECK (revision_seq > 0),
    reviewer_id uuid NULL REFERENCES users (id) ON DELETE SET NULL,
    verdict text NOT NULL CHECK (verdict IN ('approve', 'request_changes', 'comment')),
    body text NOT NULL DEFAULT '',
    created_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX mr_reviews_mr ON mr_reviews (mr_id, created_at);
CREATE INDEX mr_reviews_reviewer ON mr_reviews (reviewer_id);

-- Set for per-change revisions: same changes with the same patches as the
-- previous per-change revision.
ALTER TABLE mr_revisions ADD COLUMN same_patches bool NULL;

ALTER TABLE timeline_events DROP CONSTRAINT timeline_events_kind_check;
ALTER TABLE timeline_events ADD CONSTRAINT timeline_events_kind_check
    CHECK (kind IN ('closed', 'reopened', 'labeled', 'unlabeled', 'assigned', 'unassigned',
        'milestoned', 'demilestoned', 'renamed', 'referenced',
        'revision', 'ready', 'draft', 'source_deleted', 'source_restored', 'merged',
        'review_requested', 'review_request_removed', 'change_split', 'change_joined',
        'change_resolved', 'approvals_carried', 'approvals_stale'));
