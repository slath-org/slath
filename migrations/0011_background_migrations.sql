-- SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- Background migrations (resumable batched backfills with a persisted
-- cursor) and the history of `slath upgrade` runs.

UPDATE schema_info SET version = 11;

CREATE TABLE background_migrations (
    name text PRIMARY KEY,
    introduced_in text NOT NULL,
    state text NOT NULL DEFAULT 'pending'
        CHECK (state IN ('pending', 'running', 'paused', 'done', 'failed')),
    cursor jsonb NULL,
    processed bigint NOT NULL DEFAULT 0,
    total_estimate bigint NULL,
    last_error text NULL,
    started_at timestamptz NULL,
    finished_at timestamptz NULL,
    updated_at timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE upgrade_history (
    id uuid PRIMARY KEY,
    from_version text NULL,
    to_version text NOT NULL,
    from_schema int NOT NULL,
    to_schema int NOT NULL,
    backup_path text NULL,
    preflight jsonb NOT NULL DEFAULT '{}',
    started_at timestamptz NOT NULL DEFAULT now(),
    finished_at timestamptz NULL,
    outcome text NULL CHECK (outcome IN ('success', 'failed', 'aborted')),
    error text NULL
);
