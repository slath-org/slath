-- SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- Structured briefs (`slath-brief/2`): a brief is a list of items, each
-- with a kind (motivation, effect, risk, gap, rollout, verification), its
-- text, and optionally where in the diff it points. The four fields of
-- `slath-brief/1` become items in their order (intent as motivation,
-- approach as an effect, verification, and known gaps), and threads on a
-- field move to that field's item. Briefs were never in a release, so
-- the rows converted here are only those of development instances.

ALTER TABLE change_briefs ADD COLUMN items jsonb;
UPDATE change_briefs SET items =
    jsonb_build_array(jsonb_build_object('kind', 'motivation', 'text', intent))
    || CASE WHEN approach IS NULL THEN '[]'::jsonb
            ELSE jsonb_build_array(jsonb_build_object('kind', 'effect', 'text', approach)) END
    || CASE WHEN verification IS NULL THEN '[]'::jsonb
            ELSE jsonb_build_array(jsonb_build_object('kind', 'verification', 'text', verification)) END
    || CASE WHEN gaps IS NULL THEN '[]'::jsonb
            ELSE jsonb_build_array(jsonb_build_object('kind', 'gap', 'text', gaps)) END;

ALTER TABLE mr_threads ADD COLUMN brief_item integer NULL CHECK (brief_item >= 0);
UPDATE mr_threads t SET brief_item = CASE t.brief_field
        WHEN 'intent' THEN 0
        WHEN 'approach' THEN 1
        WHEN 'verification' THEN 1 + (b.approach IS NOT NULL)::int
        ELSE 1 + (b.approach IS NOT NULL)::int + (b.verification IS NOT NULL)::int
    END
    FROM change_briefs b
    WHERE b.id = t.brief_id;
ALTER TABLE mr_threads
    DROP CONSTRAINT mr_threads_brief,
    DROP COLUMN brief_field,
    ADD CONSTRAINT mr_threads_brief CHECK (
        (brief_id IS NULL) = (brief_item IS NULL) AND (brief_id IS NULL OR path IS NULL));

ALTER TABLE change_briefs
    DROP COLUMN intent,
    DROP COLUMN approach,
    DROP COLUMN verification,
    DROP COLUMN gaps,
    DROP CONSTRAINT change_briefs_schema_check,
    ALTER COLUMN items SET NOT NULL,
    ADD CONSTRAINT change_briefs_items CHECK (
        jsonb_typeof(items) = 'array' AND jsonb_array_length(items) >= 1);
UPDATE change_briefs SET schema = 'slath-brief/2';
ALTER TABLE change_briefs ADD CONSTRAINT change_briefs_schema_check CHECK (schema = 'slath-brief/2');

UPDATE schema_info SET version = 164;
