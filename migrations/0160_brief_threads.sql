-- SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- Review threads on one field of a change brief's edit (its intent,
-- approach, verification, or known gaps), in place of a file and lines.
-- `line_hash` and `context` hold the field's text as it was, like a code
-- anchor's lines.

ALTER TABLE mr_threads
    ADD COLUMN brief_id uuid NULL REFERENCES change_briefs (id) ON DELETE CASCADE,
    ADD COLUMN brief_field text NULL
        CHECK (brief_field IN ('intent', 'approach', 'verification', 'gaps')),
    ADD CONSTRAINT mr_threads_brief CHECK (
        (brief_id IS NULL) = (brief_field IS NULL) AND (brief_id IS NULL OR path IS NULL));
CREATE INDEX mr_threads_brief ON mr_threads (brief_id) WHERE brief_id IS NOT NULL;

UPDATE schema_info SET version = 160;
