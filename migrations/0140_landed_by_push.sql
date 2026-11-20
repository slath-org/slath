-- SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- Merge requests whose commits reached the target branch by a push, not
-- through Slath's merge, are recorded as merged with strategy 'external'.

UPDATE schema_info SET version = 140;

ALTER TABLE merge_requests DROP CONSTRAINT merge_requests_merge_strategy_check;
ALTER TABLE merge_requests ADD CONSTRAINT merge_requests_merge_strategy_check
    CHECK (merge_strategy IN ('fast-forward', 'rebase', 'merge-commit', 'squash', 'external'));
