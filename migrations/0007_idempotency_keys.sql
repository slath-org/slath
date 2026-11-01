-- SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- Idempotency keys for API requests: the stored response of a request is
-- replayed for a retry with the same key for 24 hours.

UPDATE schema_info SET version = 7;

CREATE TABLE idempotency_keys (
    principal_id uuid NOT NULL,
    key text NOT NULL CHECK (key ~ '^[A-Za-z0-9_-]{1,128}$'),
    method text NOT NULL,
    path text NOT NULL,
    request_sha256 bytea NOT NULL,
    -- NULL while the first request is still running.
    status int NULL,
    response jsonb NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (principal_id, key)
);
CREATE INDEX idempotency_keys_created_at_idx ON idempotency_keys (created_at);
