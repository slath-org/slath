-- SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- OpenID Connect sign-in: identities linked to accounts (the link key is
-- issuer and subject, never the email), single-use authorization states,
-- and first sign-ins waiting for the person to confirm a new account.

UPDATE schema_info SET version = 23;

CREATE TABLE oidc_identities (
    issuer text NOT NULL,
    subject text NOT NULL,
    user_id uuid NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    provider_key text NOT NULL,
    email text NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    last_login_at timestamptz NULL,
    PRIMARY KEY (issuer, subject)
);
CREATE INDEX oidc_identities_user ON oidc_identities (user_id);

CREATE TABLE oidc_states (
    state_hash text PRIMARY KEY,
    provider_key text NOT NULL,
    -- Hash of the browser's pre-login (or session) seed the flow is bound to.
    browser_hash text NOT NULL,
    nonce text NOT NULL,
    pkce_verifier text NOT NULL,
    purpose text NOT NULL CHECK (purpose IN ('login', 'link')),
    user_id uuid NULL REFERENCES users (id) ON DELETE CASCADE,
    next text NULL,
    expires_at timestamptz NOT NULL
);

CREATE TABLE oidc_pending (
    token_hash text PRIMARY KEY,
    provider_key text NOT NULL,
    browser_hash text NOT NULL,
    issuer text NOT NULL,
    subject text NOT NULL,
    email text NULL,
    email_verified bool NOT NULL,
    preferred_username text NULL,
    display_name text NULL,
    expires_at timestamptz NOT NULL
);
