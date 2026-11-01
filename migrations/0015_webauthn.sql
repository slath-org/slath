-- SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- Passkeys (WebAuthn credentials) and the state of ceremonies in progress.

UPDATE schema_info SET version = 15;

CREATE TABLE webauthn_credentials (
    id uuid PRIMARY KEY,
    user_id uuid NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    credential_id bytea NOT NULL UNIQUE,
    -- The credential's public key and counters (webauthn-rs `Passkey`).
    passkey jsonb NOT NULL,
    name text NOT NULL,
    last_used_at timestamptz NULL,
    created_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX webauthn_credentials_user ON webauthn_credentials (user_id);

-- A registration or authentication waiting for the browser's answer. The
-- browser holds the token; only its SHA-256 is stored.
CREATE TABLE webauthn_ceremonies (
    id_hash bytea PRIMARY KEY,
    user_id uuid NULL REFERENCES users (id) ON DELETE CASCADE,
    kind text NOT NULL CHECK (kind IN ('register', 'authenticate', 'discover')),
    state jsonb NOT NULL,
    expires_at timestamptz NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX webauthn_ceremonies_expiry ON webauthn_ceremonies (expires_at);
