-- SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- Merge request briefs: one brief per revision of a merge request, for
-- the behavioral view across a stack, beside the briefs of its changes.
-- A row is either a change version's brief (`change_id`, the version's
-- commit) or a revision's (`revision_seq`, the revision's head).

ALTER TABLE change_briefs
    ALTER COLUMN change_id DROP NOT NULL,
    ADD COLUMN revision_seq integer NULL CHECK (revision_seq >= 1),
    ADD CONSTRAINT change_briefs_target CHECK ((change_id IS NULL) <> (revision_seq IS NULL));
CREATE INDEX change_briefs_revision ON change_briefs (mr_id, revision_seq, created_at DESC, id DESC)
    WHERE revision_seq IS NOT NULL;

UPDATE schema_info SET version = 165;
