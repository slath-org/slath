-- SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- The attention gate: a bot's merge request iterates, and reaches its
-- reviewers only once it's marked ready and, unless the protection says
-- otherwise, its required checks pass on the head.

ALTER TABLE merge_requests
    ADD COLUMN attention text NOT NULL DEFAULT 'ready'
        CHECK (attention IN ('iterating', 'waiting_for_checks', 'ready'));

CREATE INDEX merge_requests_waiting_for_checks ON merge_requests (repo_id)
    WHERE attention = 'waiting_for_checks' AND state = 'open';

ALTER TABLE branch_protections
    ADD COLUMN ready_waits_for_checks bool NOT NULL DEFAULT true;

ALTER TABLE timeline_events DROP CONSTRAINT timeline_events_kind_check;
ALTER TABLE timeline_events ADD CONSTRAINT timeline_events_kind_check
    CHECK (kind IN ('closed', 'reopened', 'labeled', 'unlabeled', 'assigned', 'unassigned',
        'milestoned', 'demilestoned', 'renamed', 'referenced',
        'revision', 'ready', 'draft', 'source_deleted', 'source_restored', 'merged',
        'review_requested', 'review_request_removed', 'change_split', 'change_joined',
        'change_resolved', 'approvals_carried', 'approvals_stale', 'thread_resolved',
        'thread_reopened', 'source_published', 'maintainer_edits_allowed',
        'maintainer_edits_disallowed', 'branch_updated', 'auto_merge_set',
        'auto_merge_cancelled', 'attention_released'));

UPDATE schema_info SET version = 133;
