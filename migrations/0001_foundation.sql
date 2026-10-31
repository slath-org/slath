-- SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- Foundation schema: identity core, reserved names,
-- the job queue, audit log, and instance settings.
-- Released migrations are never edited; fix forward with a new file.

CREATE EXTENSION IF NOT EXISTS citext;

-- Latest applied migration; every migration sets it.
CREATE TABLE schema_info (
    version int NOT NULL,
    singleton bool PRIMARY KEY DEFAULT true CHECK (singleton)
);
INSERT INTO schema_info (version) VALUES (1);

CREATE TABLE namespaces (
    id uuid PRIMARY KEY,
    name citext NOT NULL UNIQUE,
    kind text NOT NULL CHECK (kind IN ('user', 'org')),
    created_at timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE users (
    id uuid PRIMARY KEY,
    namespace_id uuid NOT NULL UNIQUE REFERENCES namespaces (id),
    kind text NOT NULL DEFAULT 'human' CHECK (kind IN ('human', 'bot')),
    bot_owner_kind text NULL CHECK (bot_owner_kind IN ('user', 'org')),
    bot_owner_id uuid NULL,
    display_name text,
    email citext NULL,
    email_verified bool NOT NULL DEFAULT false,
    password_hash text NULL,
    is_admin bool NOT NULL DEFAULT false,
    state text NOT NULL DEFAULT 'active' CHECK (state IN ('active', 'suspended', 'deleted')),
    mfa_required bool NOT NULL DEFAULT false,
    avatar_blob_sha256 text NULL,
    timezone text NULL,
    theme text NOT NULL DEFAULT 'system' CHECK (theme IN ('system', 'light', 'dark')),
    diff_view text NOT NULL DEFAULT 'unified' CHECK (diff_view IN ('unified', 'split')),
    editor_mode text NOT NULL DEFAULT 'wysiwyg' CHECK (editor_mode IN ('wysiwyg', 'source')),
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    CHECK ((kind = 'bot') = (bot_owner_kind IS NOT NULL AND bot_owner_id IS NOT NULL)),
    CHECK (kind = 'human' OR password_hash IS NULL)
);
CREATE UNIQUE INDEX users_verified_email_key ON users (email) WHERE email_verified;

-- Kept in sync with slath_core::names::RESERVED_NAMES (tested in crates/db/tests).
CREATE TABLE reserved_names (
    name citext PRIMARY KEY
);
INSERT INTO reserved_names (name) VALUES
    ('-'), ('about'), ('admin'), ('api'), ('assets'), ('explore'), ('favicon.ico'),
    ('ghost'), ('git'), ('help'), ('login'), ('logout'), ('new'), ('notifications'),
    ('org'), ('orgs'), ('robots.txt'), ('root'), ('search'), ('settings'), ('signup'),
    ('slath'), ('static'), ('system'), ('user'), ('users');

-- Durable job queue. A dedupe_key is unique across all states, so
-- callers that want "at most one pending" semantics include a time bucket.
CREATE TABLE jobs (
    id bigserial PRIMARY KEY,
    queue text NOT NULL,
    kind text NOT NULL,
    payload jsonb NOT NULL DEFAULT '{}',
    dedupe_key text NULL UNIQUE,
    state text NOT NULL DEFAULT 'ready' CHECK (state IN ('ready', 'running', 'done', 'dead')),
    run_at timestamptz NOT NULL DEFAULT now(),
    attempts int NOT NULL DEFAULT 0,
    max_attempts int NOT NULL DEFAULT 8 CHECK (max_attempts > 0),
    locked_by text NULL,
    locked_until timestamptz NULL,
    last_error text NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    finished_at timestamptz NULL
);
CREATE INDEX jobs_queue_state_run_at_idx ON jobs (queue, state, run_at);
CREATE INDEX jobs_running_locked_until_idx ON jobs (locked_until) WHERE state = 'running';

-- Append-only audit log. UPDATE is never allowed; DELETE only for retention
-- purges that set `slath.audit_purge = 'on'` in their transaction.
CREATE TABLE audit_events (
    id uuid PRIMARY KEY,
    actor_id uuid NULL,
    actor_ip inet NULL,
    action text NOT NULL,
    target_kind text NOT NULL,
    target_id uuid NULL,
    repo_id uuid NULL,
    org_id uuid NULL,
    data jsonb NOT NULL DEFAULT '{}',
    created_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX audit_events_created_at_idx ON audit_events (created_at);

CREATE FUNCTION audit_events_append_only() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP = 'DELETE' AND current_setting('slath.audit_purge', true) = 'on' THEN
        RETURN OLD;
    END IF;
    RAISE EXCEPTION 'audit_events is append-only';
END;
$$;
CREATE TRIGGER audit_events_append_only
    BEFORE UPDATE OR DELETE ON audit_events
    FOR EACH ROW EXECUTE FUNCTION audit_events_append_only();

CREATE TABLE instance_settings (
    key text PRIMARY KEY,
    value jsonb NOT NULL,
    updated_at timestamptz NOT NULL DEFAULT now()
);
