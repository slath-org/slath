-- SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- Line annotations of commit statuses: findings a check reports on lines
-- of the commit. They belong to their status and never change.

CREATE TABLE status_annotations (
    status_id uuid NOT NULL REFERENCES commit_statuses (id) ON DELETE CASCADE,
    ordinal int NOT NULL CHECK (ordinal >= 0),
    path text NOT NULL CHECK (length(path) BETWEEN 1 AND 4096),
    line_start int NOT NULL CHECK (line_start > 0),
    line_end int NOT NULL,
    severity text NOT NULL CHECK (severity IN ('notice', 'warning', 'failure')),
    category text NOT NULL CHECK (category IN ('correctness', 'security', 'tests', 'dependencies',
        'scope', 'maintainability', 'docs', 'other')),
    message text NOT NULL CHECK (length(message) >= 1),
    PRIMARY KEY (status_id, ordinal),
    CHECK (line_end >= line_start)
);

UPDATE schema_info SET version = 128;
