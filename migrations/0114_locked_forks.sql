-- SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- Forks whose owner lost access to their private parent are locked: hidden
-- from everyone but site administrators until access returns, and purged
-- `deleted_repo_days` after `deleted_at` (when they were locked).

ALTER TABLE repositories DROP CONSTRAINT repositories_state_check;
ALTER TABLE repositories ADD CONSTRAINT repositories_state_check
    CHECK (state IN ('active', 'archived', 'deleting', 'locked'));

UPDATE schema_info SET version = 114;
