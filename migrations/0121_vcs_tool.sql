-- SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- Which tool's commands the UI shows a user: their setting, else what
-- their recent pushes came from (NULL: not known yet).

ALTER TABLE users
    ADD COLUMN vcs_tool text NULL CHECK (vcs_tool IN ('git', 'jj')),
    ADD COLUMN pushed_tool text NULL CHECK (pushed_tool IN ('git', 'jj'));

UPDATE schema_info SET version = 121;
