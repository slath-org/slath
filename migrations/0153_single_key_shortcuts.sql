-- SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- Whether keyboard shortcuts of one key (j, n, ?) work for the user.
-- People using speech input or who mistype can turn them off; shortcuts
-- with Ctrl or Cmd keep working.

ALTER TABLE users ADD COLUMN single_key_shortcuts boolean NOT NULL DEFAULT true;

UPDATE schema_info SET version = 153;
