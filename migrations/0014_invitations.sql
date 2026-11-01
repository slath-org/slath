-- SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- Invitation links for `auth.registration = "invite"`. Only the SHA-256
-- of each token is stored.

UPDATE schema_info SET version = 14;

CREATE TABLE invitations (
    id uuid PRIMARY KEY,
    token_hash bytea NOT NULL UNIQUE,
    created_by uuid NULL REFERENCES users (id) ON DELETE SET NULL,
    -- If set, the new account must use this address (and it counts as confirmed).
    email citext NULL,
    note text NULL,
    expires_at timestamptz NOT NULL,
    used_at timestamptz NULL,
    used_by uuid NULL REFERENCES users (id) ON DELETE SET NULL,
    revoked_at timestamptz NULL,
    created_at timestamptz NOT NULL DEFAULT now()
);
