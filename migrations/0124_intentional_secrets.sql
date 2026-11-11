-- SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- Repositories that hold secrets on purpose: a rejected push with several
-- findings is remembered so they can be allowed in one step, and an
-- import counts the findings its allowlist covers apart.

ALTER TABLE secret_rejections ADD COLUMN kind text NOT NULL DEFAULT 'first_push'
    CHECK (kind IN ('first_push', 'push'));
ALTER TABLE secret_rejections ALTER COLUMN kind DROP DEFAULT;

ALTER TABLE repo_imports ADD COLUMN secret_findings_allowed int NULL
    CHECK (secret_findings_allowed >= 0);

UPDATE schema_info SET version = 124;
