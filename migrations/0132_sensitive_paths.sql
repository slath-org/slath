-- SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- A repository's own sensitive paths: globs on top of the built-in ones.

ALTER TABLE repositories
    ADD COLUMN sensitive_paths text[] NOT NULL DEFAULT '{}'
        CHECK (cardinality(sensitive_paths) <= 100);

UPDATE schema_info SET version = 132;
