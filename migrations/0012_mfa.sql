-- SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- Two-factor authentication: recovery codes, and the pending sign-ins
-- waiting for their second factor (also used by password resets).

UPDATE schema_info SET version = 12;

CREATE TABLE recovery_codes (
    id uuid PRIMARY KEY,
    user_id uuid NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    code_hash text NOT NULL,          -- Argon2id PHC string
    used_at timestamptz NULL,
    created_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX recovery_codes_user ON recovery_codes (user_id) WHERE used_at IS NULL;

-- A sign-in (or password reset) that passed its first factor. The browser
-- holds the token; only its SHA-256 is stored.
CREATE TABLE login_challenges (
    id_hash bytea PRIMARY KEY,
    user_id uuid NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    purpose text NOT NULL CHECK (purpose IN ('login', 'sudo')),
    first_factor text NOT NULL CHECK (first_factor IN ('password', 'oidc', 'reset')),
    data jsonb NOT NULL DEFAULT '{}',
    failures int NOT NULL DEFAULT 0,
    expires_at timestamptz NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX login_challenges_expiry ON login_challenges (expires_at);
