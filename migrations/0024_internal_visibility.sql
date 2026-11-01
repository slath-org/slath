-- SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- A third repository visibility: internal repositories are readable by
-- every signed-in user and invisible to anonymous visitors.

UPDATE schema_info SET version = 24;

ALTER TABLE repositories DROP CONSTRAINT repositories_visibility_check;
ALTER TABLE repositories ADD CONSTRAINT repositories_visibility_check
    CHECK (visibility IN ('public', 'internal', 'private'));
