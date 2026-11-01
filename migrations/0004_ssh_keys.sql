-- SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- SSH public keys. A fingerprint belongs to exactly one account; lookups
-- are exact, case-sensitive matches on the unique index.

UPDATE schema_info SET version = 4;

CREATE TABLE ssh_keys (
    id uuid PRIMARY KEY,
    user_id uuid NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    title text NOT NULL CHECK (length(title) BETWEEN 1 AND 100),
    key_type text NOT NULL CHECK (key_type ~ '^[A-Za-z0-9.@-]+$'),
    public_key text NOT NULL CHECK (public_key ~ '^[A-Za-z0-9+/=]+$'),
    fingerprint_sha256 text NOT NULL UNIQUE CHECK (fingerprint_sha256 ~ '^SHA256:[A-Za-z0-9+/]{43}$'),
    last_used_at timestamptz NULL,
    created_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX ssh_keys_user_id_idx ON ssh_keys (user_id);
