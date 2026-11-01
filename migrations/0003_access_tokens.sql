-- SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- Personal access tokens. Only a display prefix and the SHA-256 of the
-- token are stored.

UPDATE schema_info SET version = 3;

CREATE TABLE access_tokens (
    id uuid PRIMARY KEY,
    user_id uuid NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    name text NOT NULL CHECK (length(name) BETWEEN 1 AND 100),
    token_prefix text NOT NULL CHECK (length(token_prefix) = 8),
    token_hash bytea NOT NULL UNIQUE CHECK (length(token_hash) = 32),
    scopes text[] NOT NULL CHECK (cardinality(scopes) > 0),
    -- NULL: every repository the user can access.
    repo_ids uuid[] NULL,
    expires_at timestamptz NULL,
    last_used_at timestamptz NULL,
    revoked_at timestamptz NULL,
    created_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX access_tokens_user_id_idx ON access_tokens (user_id);
