-- SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- Reviews of one change: a verdict on that change's version in the
-- revision it was given on. NULL is a review of the whole revision.

UPDATE schema_info SET version = 142;

ALTER TABLE mr_reviews ADD COLUMN change_id uuid NULL REFERENCES mr_changes (id) ON DELETE CASCADE;
CREATE INDEX mr_reviews_change ON mr_reviews (change_id) WHERE change_id IS NOT NULL;
