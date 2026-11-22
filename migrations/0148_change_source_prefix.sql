-- SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- The prefix of a repository's change sources: branches that open a merge
-- request when pushed and stay out of branch lists, such as the push-*
-- bookmarks of `jj git push --change`. Empty turns them off.

ALTER TABLE repositories
    ADD COLUMN change_source_prefix text NOT NULL DEFAULT 'push-'
        CHECK (change_source_prefix ~ '^([A-Za-z0-9][A-Za-z0-9._/-]{0,30}[-_/])?$');

-- Repositories that already have push-* branches keep them as plain
-- branches until an administrator turns change sources on, so nothing
-- they hold disappears from their lists or opens merge requests unasked.
UPDATE repositories r SET change_source_prefix = ''
    WHERE EXISTS (
        SELECT 1 FROM git_refs g
        WHERE g.repo_id = r.id
          AND substring(g.name FROM 1 FOR 16) = 'refs/heads/push-'::bytea
    );

UPDATE schema_info SET version = 148;
