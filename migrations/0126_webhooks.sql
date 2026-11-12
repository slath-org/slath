-- SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- Webhooks of repositories and organizations, and their deliveries. A
-- delivery keeps its ID across retries; each attempt is recorded.

CREATE TABLE webhooks (
    id uuid PRIMARY KEY,
    repo_id uuid NULL REFERENCES repositories (id) ON DELETE CASCADE,
    org_id uuid NULL REFERENCES organizations (id) ON DELETE CASCADE,
    url text NOT NULL CHECK (length(url) <= 2048),
    secret_enc bytea NOT NULL,
    key_id text NOT NULL REFERENCES encryption_keys (key_id),
    events text[] NOT NULL CHECK (cardinality(events) > 0),
    active bool NOT NULL DEFAULT true,
    -- Why it was switched off by Slath (auto-disable, principal gone).
    disabled_reason text NULL,
    consecutive_failures int NOT NULL DEFAULT 0,
    -- The principal deliveries are checked against.
    principal_id uuid NULL REFERENCES users (id) ON DELETE SET NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    CHECK ((repo_id IS NULL) <> (org_id IS NULL))
);
CREATE INDEX webhooks_repo ON webhooks (repo_id) WHERE repo_id IS NOT NULL;
CREATE INDEX webhooks_org ON webhooks (org_id) WHERE org_id IS NOT NULL;
CREATE INDEX webhooks_key_id ON webhooks (key_id);

CREATE TABLE webhook_deliveries (
    id uuid PRIMARY KEY,
    webhook_id uuid NOT NULL REFERENCES webhooks (id) ON DELETE CASCADE,
    event text NOT NULL,
    action text NULL,
    payload jsonb NOT NULL,
    state text NOT NULL DEFAULT 'pending' CHECK (state IN ('pending', 'delivered', 'failed')),
    attempts int NOT NULL DEFAULT 0,
    next_attempt_at timestamptz NULL,
    redelivery_of uuid NULL REFERENCES webhook_deliveries (id) ON DELETE SET NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    finished_at timestamptz NULL
);
CREATE INDEX webhook_deliveries_hook ON webhook_deliveries (webhook_id, created_at DESC);
CREATE INDEX webhook_deliveries_created ON webhook_deliveries (created_at);

CREATE TABLE webhook_attempts (
    delivery_id uuid NOT NULL REFERENCES webhook_deliveries (id) ON DELETE CASCADE,
    attempt int NOT NULL CHECK (attempt > 0),
    started_at timestamptz NOT NULL DEFAULT now(),
    duration_ms int NOT NULL,
    status int NULL,
    -- The response body, cut at 4 KiB.
    response text NULL CHECK (length(response) <= 4096),
    error text NULL CHECK (length(error) <= 500),
    PRIMARY KEY (delivery_id, attempt)
);

UPDATE schema_info SET version = 126;
