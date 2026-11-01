-- SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- Master keys that have encrypted secrets at rest. Metadata only: the key
-- material stays in files outside the database and backups.

UPDATE schema_info SET version = 10;

CREATE TABLE encryption_keys (
    key_id text PRIMARY KEY CHECK (key_id ~ '^[0-9a-f]{8}$'),
    created_at timestamptz NOT NULL DEFAULT now(),
    retired_at timestamptz NULL
);

-- TOTP seeds, encrypted with the master key (bound to the row by the
-- associated data). `last_used_step` rejects reuse of a code.
CREATE TABLE totp_credentials (
    user_id uuid PRIMARY KEY REFERENCES users (id) ON DELETE CASCADE,
    secret_enc bytea NOT NULL,
    key_id text NOT NULL REFERENCES encryption_keys (key_id),
    last_used_step bigint NOT NULL DEFAULT 0,
    confirmed bool NOT NULL DEFAULT false,
    created_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX totp_credentials_key_id ON totp_credentials (key_id);
