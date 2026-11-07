-- SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- Push protection allowances: a repository writer lets one finding (by
-- the secret's fingerprint) through for a limited time. Rejected first
-- pushes into empty repositories are remembered briefly, so a repository
-- administrator can accept all their findings in one step.

UPDATE schema_info SET version = 103;

CREATE TABLE secret_allowances (
    id uuid PRIMARY KEY,
    repo_id uuid NOT NULL REFERENCES repositories (id) ON DELETE CASCADE,
    fingerprint text NOT NULL CHECK (fingerprint ~ '^[0-9a-f]{32}$'),
    rule text NOT NULL CHECK (length(rule) BETWEEN 1 AND 100),
    created_by uuid NULL REFERENCES users (id) ON DELETE SET NULL,
    reason text NULL CHECK (length(reason) <= 500),
    created_at timestamptz NOT NULL DEFAULT now(),
    expires_at timestamptz NOT NULL,
    CHECK (expires_at > created_at)
);
CREATE INDEX secret_allowances_active ON secret_allowances (repo_id, fingerprint, expires_at);

CREATE TABLE secret_rejections (
    id uuid PRIMARY KEY,
    repo_id uuid NOT NULL REFERENCES repositories (id) ON DELETE CASCADE,
    -- (fingerprint, rule) of each finding.
    findings jsonb NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    expires_at timestamptz NOT NULL
);
CREATE INDEX secret_rejections_expiry ON secret_rejections (expires_at);
