-- SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- The timeline records when a fork merge request's author turns edits by
-- maintainers on or off.

ALTER TABLE timeline_events DROP CONSTRAINT timeline_events_kind_check;
ALTER TABLE timeline_events ADD CONSTRAINT timeline_events_kind_check
    CHECK (kind IN ('closed', 'reopened', 'labeled', 'unlabeled', 'assigned', 'unassigned',
        'milestoned', 'demilestoned', 'renamed', 'referenced',
        'revision', 'ready', 'draft', 'source_deleted', 'source_restored', 'merged',
        'review_requested', 'review_request_removed', 'change_split', 'change_joined',
        'change_resolved', 'approvals_carried', 'approvals_stale', 'thread_resolved',
        'thread_reopened', 'source_published', 'maintainer_edits_allowed',
        'maintainer_edits_disallowed'));

UPDATE schema_info SET version = 117;
