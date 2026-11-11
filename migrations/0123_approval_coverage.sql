-- SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- Approval coverage: at each approval, how many files the approved
-- revision's diff has and which of them the approver had viewed (with the
-- blob of that revision). The snapshot never changes.

ALTER TABLE mr_reviews ADD COLUMN files_total int NULL CHECK (files_total >= 0);

CREATE TABLE mr_review_viewed_files (
    review_id uuid NOT NULL REFERENCES mr_reviews (id) ON DELETE CASCADE,
    path text NOT NULL,
    PRIMARY KEY (review_id, path)
);

UPDATE schema_info SET version = 123;
