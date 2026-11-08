-- SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- Whether a user wants whitespace changes ignored in diffs (next to
-- users.diff_view, unified or split).

UPDATE schema_info SET version = 107;

ALTER TABLE users ADD COLUMN diff_ignore_whitespace bool NOT NULL DEFAULT false;
