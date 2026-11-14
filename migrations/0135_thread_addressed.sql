-- SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- Bots can mark a thread addressed; only people resolve threads people
-- started. Addressed isn't resolved: it satisfies nothing, and resolving,
-- reopening, or a reply by anyone else clears it.

ALTER TABLE mr_threads
    ADD COLUMN addressed_by uuid NULL REFERENCES users (id) ON DELETE SET NULL,
    ADD COLUMN addressed_at timestamptz NULL;

ALTER TABLE timeline_events DROP CONSTRAINT timeline_events_kind_check;
ALTER TABLE timeline_events ADD CONSTRAINT timeline_events_kind_check
    CHECK (kind IN ('closed', 'reopened', 'labeled', 'unlabeled', 'assigned', 'unassigned',
        'milestoned', 'demilestoned', 'renamed', 'referenced',
        'revision', 'ready', 'draft', 'source_deleted', 'source_restored', 'merged',
        'review_requested', 'review_request_removed', 'change_split', 'change_joined',
        'change_resolved', 'approvals_carried', 'approvals_stale', 'thread_resolved',
        'thread_reopened', 'source_published', 'maintainer_edits_allowed',
        'maintainer_edits_disallowed', 'branch_updated', 'auto_merge_set',
        'auto_merge_cancelled', 'attention_released', 'thread_addressed'));

UPDATE schema_info SET version = 135;
