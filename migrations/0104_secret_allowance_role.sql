-- SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- Who may allow push protection findings in a repository: any writer
-- (the default) or only its administrators.

UPDATE schema_info SET version = 104;

ALTER TABLE repositories ADD COLUMN secret_allowance_role text NOT NULL DEFAULT 'write'
    CHECK (secret_allowance_role IN ('write', 'admin'));
