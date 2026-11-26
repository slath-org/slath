-- SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- Protection rules on facts: the facts of a merge request's diff that need
-- an approval by a person, and whether every changed file must have been
-- viewed by a counting approver. Both are off by default.

ALTER TABLE branch_protections
    ADD COLUMN fact_rules text[] NOT NULL DEFAULT '{}'
        CHECK (fact_rules <@ ARRAY['outside_scope', 'dependencies', 'deleted_tests',
                                   'added_binaries', 'mode_changes', 'secrets']::text[]),
    ADD COLUMN require_viewed_files boolean NOT NULL DEFAULT false;

UPDATE schema_info SET version = 163;
