-- SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- A timeline event for a partial landing whose rest of the stack couldn't
-- be rebased because someone pushed the branch meanwhile.

UPDATE schema_info SET version = 143;

ALTER TABLE timeline_events DROP CONSTRAINT timeline_events_kind_check;
ALTER TABLE timeline_events ADD CONSTRAINT timeline_events_kind_check
    CHECK (kind IN ('closed', 'reopened', 'labeled', 'unlabeled', 'assigned', 'unassigned',
        'milestoned', 'demilestoned', 'renamed', 'referenced',
        'revision', 'ready', 'draft', 'source_deleted', 'source_restored', 'merged',
        'review_requested', 'review_request_removed', 'change_split', 'change_joined',
        'change_resolved', 'approvals_carried', 'approvals_stale', 'thread_resolved',
        'thread_reopened', 'source_published', 'maintainer_edits_allowed',
        'maintainer_edits_disallowed', 'branch_updated', 'auto_merge_set',
        'auto_merge_cancelled', 'attention_released', 'thread_addressed', 'changes_landed',
        'possibly_landed', 'rest_not_rebased'));
