-- SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- Repository maintenance bookkeeping: pushes since the last run, when it
-- last ran, a run counter (part of the job's dedupe key), and a lease that
-- keeps two runs on one repository from overlapping.

UPDATE schema_info SET version = 8;

ALTER TABLE repositories
    ADD COLUMN pushes_since_maintenance int NOT NULL DEFAULT 0,
    ADD COLUMN maintained_at timestamptz NULL,
    ADD COLUMN maintenance_runs int NOT NULL DEFAULT 0,
    ADD COLUMN maintenance_lease_until timestamptz NULL;

CREATE INDEX repositories_maintenance_due_idx ON repositories (maintained_at)
    WHERE pushes_since_maintenance > 0 AND state <> 'deleting';
