-- SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- Finding threads: review threads made from a commit status's line
-- annotations, one per finding, context, and merge request revision.

CREATE TABLE mr_thread_findings (
    thread_id uuid PRIMARY KEY REFERENCES mr_threads (id) ON DELETE CASCADE,
    mr_id uuid NOT NULL REFERENCES merge_requests (id) ON DELETE CASCADE,
    revision_seq int NOT NULL CHECK (revision_seq > 0),
    context text NOT NULL CHECK (context ~ '^[A-Za-z0-9 ._/:-]{1,100}$'),
    -- SHA-256 of the context and the annotation's fields.
    key text NOT NULL CHECK (key ~ '^[0-9a-f]{64}$'),
    severity text NOT NULL CHECK (severity IN ('notice', 'warning', 'failure')),
    category text NOT NULL,
    -- The status that first reported it.
    status_id uuid NULL REFERENCES commit_statuses (id) ON DELETE SET NULL,
    -- Set while it's resolved because a later status stopped reporting it.
    resolved_by_status uuid NULL REFERENCES commit_statuses (id) ON DELETE SET NULL,
    UNIQUE (mr_id, revision_seq, context, key)
);

UPDATE schema_info SET version = 129;
