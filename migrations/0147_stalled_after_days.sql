-- SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- How many days without activity make an open merge request stalled, per
-- repository (the merge request list's Stalled view).

ALTER TABLE repositories
    ADD COLUMN stalled_after_days smallint NOT NULL DEFAULT 14
        CHECK (stalled_after_days BETWEEN 1 AND 365);

UPDATE schema_info SET version = 147;
