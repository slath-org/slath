-- SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- Merge requests from forks: what the target's push protection found in a
-- source head it didn't fetch (redacted findings, or why the scan couldn't
-- finish). NULL when the latest source head passed.

ALTER TABLE merge_requests ADD COLUMN source_findings jsonb NULL;

UPDATE schema_info SET version = 116;
