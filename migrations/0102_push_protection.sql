-- SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- Push protection: each repository's mode (the site configuration can
-- enforce a stricter minimum), and what an import's report-only scan found.

UPDATE schema_info SET version = 102;

ALTER TABLE repositories ADD COLUMN push_protection text NOT NULL DEFAULT 'block'
    CHECK (push_protection IN ('block', 'warn', 'off'));
ALTER TABLE repo_imports ADD COLUMN secret_findings int NULL CHECK (secret_findings >= 0);
