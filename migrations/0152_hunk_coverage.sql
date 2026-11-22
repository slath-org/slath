-- SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- An approval's coverage counts hunks too: how many the approved diff has
-- and how many of them the approver had viewed. Both stay NULL when the
-- diff exceeded the diff limits (unknown, never estimated) and for
-- approvals recorded before hunks were counted.

ALTER TABLE mr_reviews
    ADD COLUMN hunks_total int NULL CHECK (hunks_total >= 0),
    ADD COLUMN hunks_viewed int NULL CHECK (hunks_viewed >= 0),
    ADD CONSTRAINT mr_reviews_hunks_check CHECK (
        (hunks_total IS NULL AND hunks_viewed IS NULL)
        OR (hunks_total IS NOT NULL AND hunks_viewed IS NOT NULL AND hunks_viewed <= hunks_total));

UPDATE schema_info SET version = 152;
