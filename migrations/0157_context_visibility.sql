-- SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- Who sees a repository's agent context records, and how long they are
-- kept. `writers` (the default) shows them to people with push
-- permission, the person who asked for the work, and the uploader's
-- owner; `readers` to everyone who can read the code; `off` refuses new
-- records. A retention of NULL keeps records until they are deleted.

ALTER TABLE repositories
    ADD COLUMN context_visibility text NOT NULL DEFAULT 'writers'
        CHECK (context_visibility IN ('writers', 'readers', 'off')),
    ADD COLUMN context_retention_days int NULL
        CHECK (context_retention_days BETWEEN 1 AND 3650);

UPDATE schema_info SET version = 157;
