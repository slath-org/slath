-- SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- Browser sessions and login rate limiting.

UPDATE schema_info SET version = 2;

-- The cookie holds 32 random bytes; only their SHA-256 is stored.
CREATE TABLE sessions (
    id_hash bytea PRIMARY KEY CHECK (length(id_hash) = 32),
    user_id uuid NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    created_at timestamptz NOT NULL DEFAULT now(),
    last_seen_at timestamptz NOT NULL DEFAULT now(),
    expires_at timestamptz NOT NULL,
    auth_level text NOT NULL CHECK (auth_level IN ('password', 'mfa', 'passkey', 'oidc')),
    sudo_until timestamptz NULL,
    ip inet NULL,
    user_agent text NULL CHECK (length(user_agent) <= 512)
);
CREATE INDEX sessions_user_id_idx ON sessions (user_id);
CREATE INDEX sessions_expires_at_idx ON sessions (expires_at);

-- Failed sign-ins per key (`account:<name>` or `ip:<address>`) and
-- 15-minute window.
CREATE TABLE login_attempts (
    key text NOT NULL CHECK (length(key) <= 200),
    window_start timestamptz NOT NULL,
    count int NOT NULL DEFAULT 0,
    PRIMARY KEY (key, window_start)
);
CREATE INDEX login_attempts_window_start_idx ON login_attempts (window_start);
