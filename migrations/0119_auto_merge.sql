-- SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- Merge when ready: finding the merge requests that have it set, and the
-- timeline entries for setting and cancelling it.

CREATE INDEX merge_requests_auto_merge ON merge_requests (repo_id)
    WHERE auto_merge_strategy IS NOT NULL;

ALTER TABLE timeline_events DROP CONSTRAINT timeline_events_kind_check;
ALTER TABLE timeline_events ADD CONSTRAINT timeline_events_kind_check
    CHECK (kind IN ('closed', 'reopened', 'labeled', 'unlabeled', 'assigned', 'unassigned',
        'milestoned', 'demilestoned', 'renamed', 'referenced',
        'revision', 'ready', 'draft', 'source_deleted', 'source_restored', 'merged',
        'review_requested', 'review_request_removed', 'change_split', 'change_joined',
        'change_resolved', 'approvals_carried', 'approvals_stale', 'thread_resolved',
        'thread_reopened', 'source_published', 'maintainer_edits_allowed',
        'maintainer_edits_disallowed', 'branch_updated', 'auto_merge_set',
        'auto_merge_cancelled'));

UPDATE schema_info SET version = 119;
