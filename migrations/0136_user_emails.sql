-- SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- An account's addresses besides its primary one (users.email), each
-- confirmed by an emailed link or by an administrator. A confirmed
-- address belongs to one account: the service checks both tables under a
-- lock when an address is confirmed. Choosing another primary address
-- swaps it with users.email, so an account always has at most one.

CREATE TABLE user_emails (
    user_id uuid NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    email citext NOT NULL,
    verified bool NOT NULL DEFAULT false,
    created_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (user_id, email)
);
CREATE UNIQUE INDEX user_emails_verified_key ON user_emails (email) WHERE verified;

UPDATE schema_info SET version = 136;
