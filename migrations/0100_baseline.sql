-- SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- The baseline schema: where every supported installation starts. It is
-- the schema of the 0.1/0.2 previews collapsed into one migration, and
-- 0.3.0, the first supported release, builds on it. Schema numbers start
-- at 100 here; a database with a lower number comes from a preview, which
-- can't be upgraded.
--
-- Released migrations are never edited; fix forward with a new file.

-- ==========================================================================
-- Foundation schema: identity core, reserved names,
-- the job queue, audit log, and instance settings.

CREATE EXTENSION IF NOT EXISTS citext;
CREATE EXTENSION IF NOT EXISTS pg_trgm;

-- Latest applied migration; every migration sets it.
CREATE TABLE schema_info (
    version int NOT NULL,
    singleton bool PRIMARY KEY DEFAULT true CHECK (singleton)
);
INSERT INTO schema_info (version) VALUES (100);

-- Content-addressed blobs (files under <data_dir>/blobs/). A blob's refcount
-- counts the rows that use it and is kept by triggers, so every way a row
-- disappears (including cascades from a deleted repository) releases its
-- blob. Blobs at refcount 0 are removed by the collector after a grace
-- period.
CREATE TABLE blobs (
    sha256 text PRIMARY KEY CHECK (sha256 ~ '^[0-9a-f]{64}$'),
    size_bytes bigint NOT NULL CHECK (size_bytes >= 0),
    refcount int NOT NULL DEFAULT 0 CHECK (refcount >= 0),
    unreferenced_since timestamptz NULL DEFAULT now(),
    created_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX blobs_unreferenced ON blobs (unreferenced_since) WHERE refcount = 0;

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
    avatar_blob_sha256 text NULL CONSTRAINT users_avatar_blob_fkey REFERENCES blobs (sha256),
    timezone text NULL,
    theme text NOT NULL DEFAULT 'system' CHECK (theme IN ('system', 'light', 'dark')),
    diff_view text NOT NULL DEFAULT 'unified' CHECK (diff_view IN ('unified', 'split')),
    editor_mode text NOT NULL DEFAULT 'wysiwyg' CHECK (editor_mode IN ('wysiwyg', 'source')),
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    -- Appearance next to `theme`: layout density and code font size in pixels.
    density text NOT NULL DEFAULT 'comfortable'
        CHECK (density IN ('comfortable', 'compact')),
    code_font_size smallint NOT NULL DEFAULT 14
        CHECK (code_font_size BETWEEN 12 AND 20),
    email_notifications bool NOT NULL DEFAULT true,
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

-- ==========================================================================
-- Browser sessions and login rate limiting.

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

-- ==========================================================================
-- Personal access tokens. Only a display prefix and the SHA-256 of the
-- token are stored.

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

-- ==========================================================================
-- SSH public keys. A fingerprint belongs to exactly one account; lookups
-- are exact, case-sensitive matches on the unique index.

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

-- ==========================================================================
-- Organizations and their members. Organizations share the namespace table
-- with users, so names never collide (case-insensitively).

CREATE TABLE organizations (
    id uuid PRIMARY KEY,
    namespace_id uuid NOT NULL UNIQUE REFERENCES namespaces (id),
    display_name text NULL CHECK (length(display_name) <= 100),
    description text NULL CHECK (length(description) <= 500),
    default_member_role text NOT NULL DEFAULT 'none' CHECK (default_member_role IN ('none', 'read')),
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    require_mfa bool NOT NULL DEFAULT false,
    avatar_blob_sha256 text NULL REFERENCES blobs (sha256)
);

CREATE TABLE org_members (
    org_id uuid NOT NULL REFERENCES organizations (id) ON DELETE CASCADE,
    user_id uuid NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    role text NOT NULL CHECK (role IN ('owner', 'member')),
    created_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (org_id, user_id)
);
CREATE INDEX org_members_user_id_idx ON org_members (user_id);

-- ==========================================================================
-- Repositories, rename redirects, collaborators, and the derived ref
-- snapshot with its push history (rebuildable from Git).

CREATE TABLE repositories (
    id uuid PRIMARY KEY,
    namespace_id uuid NOT NULL REFERENCES namespaces (id),
    name citext NOT NULL,
    description text NULL CHECK (length(description) <= 500),
    visibility text NOT NULL CHECK (visibility IN ('public', 'internal', 'private')),
    default_branch text NOT NULL DEFAULT 'main' CHECK (length(default_branch) BETWEEN 1 AND 255),
    state text NOT NULL DEFAULT 'active' CHECK (state IN ('active', 'archived', 'deleting')),
    fork_parent_id uuid NULL REFERENCES repositories (id) ON DELETE SET NULL,
    fork_network_id uuid NOT NULL,
    allow_forking bool NOT NULL DEFAULT true,
    trust_bot_runs bool NOT NULL DEFAULT false,
    auto_approve_external_for text NOT NULL DEFAULT 'none',
    issues_enabled bool NOT NULL DEFAULT true,
    merge_requests_enabled bool NOT NULL DEFAULT true,
    size_bytes bigint NOT NULL DEFAULT 0,
    next_item_number int NOT NULL DEFAULT 1,
    merge_strategies text[] NOT NULL DEFAULT '{fast-forward,rebase,merge-commit,squash}',
    default_merge_strategy text NOT NULL DEFAULT 'rebase',
    delete_source_branch_default bool NOT NULL DEFAULT true,
    archived_reason text NULL CHECK (archived_reason IN ('user', 'lost_parent_access')),
    deleted_at timestamptz NULL,
    pushed_at timestamptz NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    -- Maintenance bookkeeping: pushes since the last run, when it last ran,
    -- a run counter (part of the job's dedupe key), and a lease that keeps
    -- two runs on one repository from overlapping.
    pushes_since_maintenance int NOT NULL DEFAULT 0,
    maintained_at timestamptz NULL,
    maintenance_runs int NOT NULL DEFAULT 0,
    maintenance_lease_until timestamptz NULL,
    UNIQUE (namespace_id, name)
);
CREATE INDEX repositories_fork_parent_id_idx ON repositories (fork_parent_id);
CREATE INDEX repositories_fork_network_id_idx ON repositories (fork_network_id);
CREATE INDEX repositories_public_idx ON repositories (updated_at DESC) WHERE visibility = 'public' AND state <> 'deleting';

-- Old names after a rename; resolved only for subjects who can read the
-- repository, so a redirect never reveals a private repository's new name.
CREATE TABLE repo_redirects (
    namespace_id uuid NOT NULL REFERENCES namespaces (id),
    name citext NOT NULL,
    repo_id uuid NOT NULL REFERENCES repositories (id) ON DELETE CASCADE,
    created_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (namespace_id, name)
);

CREATE TABLE repo_collaborators (
    repo_id uuid NOT NULL REFERENCES repositories (id) ON DELETE CASCADE,
    user_id uuid NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    role text NOT NULL CHECK (role IN ('read', 'write', 'maintain', 'admin')),
    created_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (repo_id, user_id)
);
CREATE INDEX repo_collaborators_user_id_idx ON repo_collaborators (user_id);

CREATE TABLE git_refs (
    repo_id uuid NOT NULL REFERENCES repositories (id) ON DELETE CASCADE,
    name bytea NOT NULL,
    oid text NOT NULL CHECK (oid ~ '^[0-9a-f]{40}$'),
    updated_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (repo_id, name)
);

CREATE TABLE push_events (
    id uuid PRIMARY KEY,
    repo_id uuid NOT NULL REFERENCES repositories (id) ON DELETE CASCADE,
    pusher_id uuid NULL REFERENCES users (id) ON DELETE SET NULL,
    transport text NOT NULL CHECK (transport IN ('http', 'ssh', 'internal', 'reconcile')),
    created_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX push_events_repo_id_idx ON push_events (repo_id, created_at DESC);

CREATE TABLE ref_updates (
    push_id uuid NOT NULL REFERENCES push_events (id) ON DELETE CASCADE,
    seq int NOT NULL,
    name bytea NOT NULL,
    old_oid text NULL CHECK (old_oid ~ '^[0-9a-f]{40}$'),
    new_oid text NULL CHECK (new_oid ~ '^[0-9a-f]{40}$'),
    PRIMARY KEY (push_id, seq)
);
CREATE INDEX repositories_maintenance_due_idx ON repositories (maintained_at)
    WHERE pushes_since_maintenance > 0 AND state <> 'deleting';
CREATE INDEX repositories_name_trgm ON repositories USING gin ((name::text) gin_trgm_ops);
CREATE INDEX repositories_description_trgm ON repositories USING gin (description gin_trgm_ops);

-- ==========================================================================
-- Idempotency keys for API requests: the stored response of a request is
-- replayed for a retry with the same key for 24 hours.

CREATE TABLE idempotency_keys (
    principal_id uuid NOT NULL,
    key text NOT NULL CHECK (key ~ '^[A-Za-z0-9_-]{1,128}$'),
    method text NOT NULL,
    path text NOT NULL,
    request_sha256 bytea NOT NULL,
    -- NULL while the first request is still running.
    status int NULL,
    response jsonb NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (principal_id, key)
);
CREATE INDEX idempotency_keys_created_at_idx ON idempotency_keys (created_at);

-- ==========================================================================
-- Master keys that have encrypted secrets at rest. Metadata only: the key
-- material stays in files outside the database and backups.

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

-- ==========================================================================
-- Background migrations (resumable batched backfills with a persisted
-- cursor) and the history of `slath upgrade` runs.

CREATE TABLE background_migrations (
    name text PRIMARY KEY,
    introduced_in text NOT NULL,
    state text NOT NULL DEFAULT 'pending'
        CHECK (state IN ('pending', 'running', 'paused', 'done', 'failed')),
    cursor jsonb NULL,
    processed bigint NOT NULL DEFAULT 0,
    total_estimate bigint NULL,
    last_error text NULL,
    started_at timestamptz NULL,
    finished_at timestamptz NULL,
    updated_at timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE upgrade_history (
    id uuid PRIMARY KEY,
    from_version text NULL,
    to_version text NOT NULL,
    from_schema int NOT NULL,
    to_schema int NOT NULL,
    backup_path text NULL,
    preflight jsonb NOT NULL DEFAULT '{}',
    started_at timestamptz NOT NULL DEFAULT now(),
    finished_at timestamptz NULL,
    outcome text NULL CHECK (outcome IN ('success', 'failed', 'aborted')),
    error text NULL
);

-- ==========================================================================
-- Two-factor authentication: recovery codes, and the pending sign-ins
-- waiting for their second factor (also used by password resets).

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

-- ==========================================================================
-- Single-use emailed links: password resets and email verification. Only
-- the SHA-256 of each token is stored.

CREATE TABLE password_resets (
    token_hash bytea PRIMARY KEY,
    user_id uuid NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    expires_at timestamptz NOT NULL,
    used_at timestamptz NULL,
    created_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX password_resets_user ON password_resets (user_id);

CREATE TABLE email_verifications (
    token_hash bytea PRIMARY KEY,
    user_id uuid NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    email citext NOT NULL,
    expires_at timestamptz NOT NULL,
    used_at timestamptz NULL,
    created_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX email_verifications_user ON email_verifications (user_id);

-- ==========================================================================
-- Invitation links for `auth.registration = "invite"`. Only the SHA-256
-- of each token is stored.

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

-- ==========================================================================
-- Passkeys (WebAuthn credentials) and the state of ceremonies in progress.

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

-- ==========================================================================
-- Teams with repository access, the organization MFA policy, and one SQL
-- function that decides whether a user can read a repository through a
-- grant. List and search queries filter with it, so visibility rules live
-- in one place (crates/core/src/authz.rs `effective_role` is the same
-- rule for one repository; tests compare them).

CREATE TABLE teams (
    id uuid PRIMARY KEY,
    org_id uuid NOT NULL REFERENCES organizations (id) ON DELETE CASCADE,
    name citext NOT NULL,
    description text NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (org_id, name)
);

CREATE TABLE team_members (
    team_id uuid NOT NULL REFERENCES teams (id) ON DELETE CASCADE,
    user_id uuid NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    PRIMARY KEY (team_id, user_id)
);
CREATE INDEX team_members_user ON team_members (user_id);

CREATE TABLE repo_team_access (
    repo_id uuid NOT NULL REFERENCES repositories (id) ON DELETE CASCADE,
    team_id uuid NOT NULL REFERENCES teams (id) ON DELETE CASCADE,
    role text NOT NULL CHECK (role IN ('read', 'write', 'maintain', 'admin')),
    PRIMARY KEY (repo_id, team_id)
);
CREATE INDEX repo_team_access_team ON repo_team_access (team_id);

-- The user has a confirmed second factor.
CREATE FUNCTION slath_has_mfa(viewer uuid) RETURNS bool
LANGUAGE sql STABLE AS $$
    SELECT EXISTS (SELECT 1 FROM totp_credentials WHERE user_id = viewer AND confirmed)
        OR EXISTS (SELECT 1 FROM webauthn_credentials WHERE user_id = viewer)
$$;

-- True if `viewer` has any role on `repo` through a grant (collaborator,
-- team, organization ownership or membership, owning the namespace). An
-- organization requiring MFA blocks all of that for users without it.
-- Public visibility and site administration are checked by the caller.
CREATE FUNCTION slath_can_read(repo uuid, viewer uuid) RETURNS bool
LANGUAGE sql STABLE AS $$
    SELECT viewer IS NOT NULL AND EXISTS (
        SELECT 1 FROM repositories r
        LEFT JOIN organizations o ON o.namespace_id = r.namespace_id
        WHERE r.id = repo
          AND NOT (COALESCE(o.require_mfa, false) AND NOT slath_has_mfa(viewer))
          AND (
               EXISTS (SELECT 1 FROM repo_collaborators c WHERE c.repo_id = r.id AND c.user_id = viewer)
            OR EXISTS (SELECT 1 FROM users u WHERE u.id = viewer AND u.namespace_id = r.namespace_id)
            OR EXISTS (SELECT 1 FROM org_members m WHERE m.org_id = o.id AND m.user_id = viewer
                         AND (m.role = 'owner' OR o.default_member_role <> 'none'))
            OR EXISTS (SELECT 1 FROM repo_team_access a JOIN team_members tm ON tm.team_id = a.team_id
                       WHERE a.repo_id = r.id AND tm.user_id = viewer)
          )
    )
$$;

-- ==========================================================================
-- Branch protection rules. Columns for merge requests (approvals, status
-- contexts, threads) are used from the merge request milestone on.

CREATE TABLE branch_protections (
    id uuid PRIMARY KEY,
    repo_id uuid NOT NULL REFERENCES repositories (id) ON DELETE CASCADE,
    -- Glob on the short branch name: `*` matches within a path segment.
    pattern text NOT NULL CHECK (length(pattern) BETWEEN 1 AND 255),
    block_force_push bool NOT NULL DEFAULT true,
    block_deletion bool NOT NULL DEFAULT true,
    require_mr bool NOT NULL DEFAULT false,
    required_approvals int NOT NULL DEFAULT 1 CHECK (required_approvals BETWEEN 0 AND 20),
    required_status_contexts text[] NOT NULL DEFAULT '{}',
    push_role text NOT NULL DEFAULT 'write'
        CHECK (push_role IN ('write', 'maintain', 'admin', 'nobody')),
    dismiss_stale_approvals bool NOT NULL DEFAULT true,
    statuses_on_head_ok bool NOT NULL DEFAULT true,
    require_resolved_threads bool NOT NULL DEFAULT false,
    bots_count_as_approvers bool NOT NULL DEFAULT false,
    bot_merge_allowlist uuid[] NOT NULL DEFAULT '{}',
    require_up_to_date bool NOT NULL DEFAULT false,
    created_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (repo_id, pattern)
);

-- ==========================================================================
-- Issues with comments, timeline, labels, milestones, and assignees.
-- Issue numbers come from repositories.next_item_number, shared with merge
-- requests, in the transaction that inserts the issue.

CREATE TABLE milestones (
    id uuid PRIMARY KEY,
    repo_id uuid NOT NULL REFERENCES repositories (id) ON DELETE CASCADE,
    title citext NOT NULL CHECK (length(title) BETWEEN 1 AND 100),
    description text NULL,
    due_on date NULL,
    state text NOT NULL DEFAULT 'open' CHECK (state IN ('open', 'closed')),
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (repo_id, title)
);

CREATE TABLE labels (
    id uuid PRIMARY KEY,
    repo_id uuid NOT NULL REFERENCES repositories (id) ON DELETE CASCADE,
    name citext NOT NULL CHECK (length(name) BETWEEN 1 AND 50),
    color char(6) NOT NULL CHECK (color ~ '^[0-9a-f]{6}$'),
    description text NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (repo_id, name)
);

CREATE TABLE issues (
    id uuid PRIMARY KEY,
    repo_id uuid NOT NULL REFERENCES repositories (id) ON DELETE CASCADE,
    number int NOT NULL CHECK (number > 0),
    author_id uuid NULL REFERENCES users (id) ON DELETE SET NULL,
    title text NOT NULL CHECK (length(title) BETWEEN 1 AND 256),
    body text NOT NULL DEFAULT '',
    state text NOT NULL DEFAULT 'open' CHECK (state IN ('open', 'closed')),
    state_reason text NULL CHECK (state_reason IN ('completed', 'not_planned', 'duplicate')),
    milestone_id uuid NULL REFERENCES milestones (id) ON DELETE SET NULL,
    closed_at timestamptz NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    search tsvector GENERATED ALWAYS AS (
        setweight(to_tsvector('simple', title), 'A') || setweight(to_tsvector('simple', body), 'B')
    ) STORED,
    UNIQUE (repo_id, number)
);
CREATE INDEX issues_search ON issues USING gin (search);
CREATE INDEX issues_repo_state ON issues (repo_id, state, updated_at DESC);
CREATE INDEX issues_author ON issues (author_id);

CREATE TABLE issue_labels (
    issue_id uuid NOT NULL REFERENCES issues (id) ON DELETE CASCADE,
    label_id uuid NOT NULL REFERENCES labels (id) ON DELETE CASCADE,
    PRIMARY KEY (issue_id, label_id)
);
CREATE INDEX issue_labels_label ON issue_labels (label_id);

CREATE TABLE issue_assignees (
    issue_id uuid NOT NULL REFERENCES issues (id) ON DELETE CASCADE,
    user_id uuid NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    PRIMARY KEY (issue_id, user_id)
);
CREATE INDEX issue_assignees_user ON issue_assignees (user_id);

CREATE TABLE comments (
    id uuid PRIMARY KEY,
    repo_id uuid NOT NULL REFERENCES repositories (id) ON DELETE CASCADE,
    subject_kind text NOT NULL CHECK (subject_kind IN ('issue', 'merge_request')),
    subject_id uuid NOT NULL,
    author_id uuid NULL REFERENCES users (id) ON DELETE SET NULL,
    body text NOT NULL,
    edited_at timestamptz NULL,
    deleted_at timestamptz NULL,
    -- Hidden by a moderator (maintain role); the author still sees it.
    hidden_at timestamptz NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    search tsvector GENERATED ALWAYS AS (to_tsvector('simple', body)) STORED
);
CREATE INDEX comments_search ON comments USING gin (search);
CREATE INDEX comments_subject ON comments (subject_kind, subject_id, created_at);

CREATE TABLE timeline_events (
    id uuid PRIMARY KEY,
    repo_id uuid NOT NULL REFERENCES repositories (id) ON DELETE CASCADE,
    subject_kind text NOT NULL CHECK (subject_kind IN ('issue', 'merge_request')),
    subject_id uuid NOT NULL,
    actor_id uuid NULL REFERENCES users (id) ON DELETE SET NULL,
    kind text NOT NULL CHECK (kind IN ('closed', 'reopened', 'labeled', 'unlabeled', 'assigned',
        'unassigned', 'milestoned', 'demilestoned', 'renamed', 'referenced')),
    data jsonb NOT NULL DEFAULT '{}',
    created_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX timeline_events_subject ON timeline_events (subject_kind, subject_id, created_at);

-- ==========================================================================
-- Issue attachments and the triggers that keep blob refcounts (blobs and
-- avatars are defined above).

CREATE TABLE attachments (
    id uuid PRIMARY KEY,
    repo_id uuid NOT NULL REFERENCES repositories (id) ON DELETE CASCADE,
    uploader_id uuid NULL REFERENCES users (id) ON DELETE SET NULL,
    blob_sha256 text NOT NULL REFERENCES blobs (sha256),
    filename text NOT NULL CHECK (length(filename) BETWEEN 1 AND 255),
    content_type text NOT NULL,
    size_bytes bigint NOT NULL CHECK (size_bytes >= 0),
    created_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX attachments_repo ON attachments (repo_id, created_at);
CREATE INDEX attachments_blob ON attachments (blob_sha256);

CREATE FUNCTION slath_blob_ref(sha text, delta int) RETURNS void
LANGUAGE sql AS $$
    UPDATE blobs
       SET refcount = refcount + delta,
           unreferenced_since = CASE WHEN refcount + delta = 0 THEN now() ELSE NULL END
     WHERE sha256 = sha;
$$;

CREATE FUNCTION slath_attachment_blob_ref() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP = 'INSERT' THEN
        PERFORM slath_blob_ref(NEW.blob_sha256, 1);
    ELSIF TG_OP = 'DELETE' THEN
        PERFORM slath_blob_ref(OLD.blob_sha256, -1);
    END IF;
    RETURN NULL;
END;
$$;

CREATE TRIGGER attachments_blob_ref AFTER INSERT OR DELETE ON attachments
    FOR EACH ROW EXECUTE FUNCTION slath_attachment_blob_ref();

CREATE FUNCTION slath_avatar_blob_ref() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP IN ('UPDATE', 'DELETE') AND OLD.avatar_blob_sha256 IS NOT NULL
       AND (TG_OP = 'DELETE' OR OLD.avatar_blob_sha256 IS DISTINCT FROM NEW.avatar_blob_sha256) THEN
        PERFORM slath_blob_ref(OLD.avatar_blob_sha256, -1);
    END IF;
    IF TG_OP IN ('UPDATE', 'INSERT') AND NEW.avatar_blob_sha256 IS NOT NULL
       AND (TG_OP = 'INSERT' OR OLD.avatar_blob_sha256 IS DISTINCT FROM NEW.avatar_blob_sha256) THEN
        PERFORM slath_blob_ref(NEW.avatar_blob_sha256, 1);
    END IF;
    RETURN NULL;
END;
$$;

CREATE TRIGGER users_avatar_blob_ref AFTER INSERT OR UPDATE OF avatar_blob_sha256 OR DELETE ON users
    FOR EACH ROW EXECUTE FUNCTION slath_avatar_blob_ref();
CREATE TRIGGER organizations_avatar_blob_ref AFTER INSERT OR UPDATE OF avatar_blob_sha256 OR DELETE ON organizations
    FOR EACH ROW EXECUTE FUNCTION slath_avatar_blob_ref();

-- Recomputes every refcount from the referencing rows. A restore loads
-- tables with the triggers active, so it calls this afterwards.
CREATE FUNCTION slath_blob_recount() RETURNS void
LANGUAGE sql AS $$
    WITH uses AS (
        SELECT blob_sha256 AS sha, count(*) AS n FROM attachments GROUP BY blob_sha256
        UNION ALL
        SELECT avatar_blob_sha256, count(*) FROM users WHERE avatar_blob_sha256 IS NOT NULL GROUP BY 1
        UNION ALL
        SELECT avatar_blob_sha256, count(*) FROM organizations WHERE avatar_blob_sha256 IS NOT NULL GROUP BY 1
    ), total AS (SELECT sha, sum(n)::int AS n FROM uses GROUP BY sha)
    UPDATE blobs b
       SET refcount = COALESCE(t.n, 0),
           unreferenced_since = CASE WHEN COALESCE(t.n, 0) = 0
                                     THEN COALESCE(b.unreferenced_since, now()) END
      FROM blobs b2 LEFT JOIN total t ON t.sha = b2.sha256
     WHERE b.sha256 = b2.sha256;
$$;

-- ==========================================================================
-- Notifications: subscriptions (watching or ignoring a repository or a
-- thread) and one inbox row per user and thread.

CREATE TABLE subscriptions (
    user_id uuid NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    subject_kind text NOT NULL CHECK (subject_kind IN ('repo', 'issue', 'merge_request')),
    subject_id uuid NOT NULL,
    -- The repository of the subject (the subject itself for 'repo'), so
    -- deleting a repository removes its subscriptions.
    repo_id uuid NOT NULL REFERENCES repositories (id) ON DELETE CASCADE,
    state text NOT NULL CHECK (state IN ('watching', 'ignoring')),
    created_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (user_id, subject_kind, subject_id)
);
CREATE INDEX subscriptions_subject ON subscriptions (subject_kind, subject_id, state);

CREATE TABLE notifications (
    id uuid PRIMARY KEY,
    user_id uuid NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    repo_id uuid NOT NULL REFERENCES repositories (id) ON DELETE CASCADE,
    subject_kind text NOT NULL CHECK (subject_kind IN ('issue', 'merge_request')),
    subject_id uuid NOT NULL,
    reason text NOT NULL CHECK (reason IN ('author', 'assigned', 'mentioned', 'review_requested',
        'mr_ready', 'auto_merge_result', 'commented', 'watching', 'state_change', 'ci_failed')),
    unread bool NOT NULL DEFAULT true,
    updated_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (user_id, subject_kind, subject_id)
);
CREATE INDEX notifications_inbox ON notifications (user_id, updated_at DESC);
CREATE INDEX notifications_unread ON notifications (user_id) WHERE unread;

-- ==========================================================================
-- Repository imports from other servers: one row per imported repository
-- with its progress and the report shown afterwards. The URL is stored
-- without credentials.

CREATE TABLE repo_imports (
    repo_id uuid PRIMARY KEY REFERENCES repositories (id) ON DELETE CASCADE,
    url text NOT NULL,
    requested_by uuid NULL REFERENCES users (id) ON DELETE SET NULL,
    state text NOT NULL DEFAULT 'queued' CHECK (state IN ('queued', 'running', 'done', 'failed')),
    error text NULL,
    branches int NULL,
    tags int NULL,
    size_bytes bigint NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    started_at timestamptz NULL,
    finished_at timestamptz NULL
);
CREATE INDEX repo_imports_unfinished ON repo_imports (state) WHERE state IN ('queued', 'running');

-- ==========================================================================
-- OpenID Connect sign-in: identities linked to accounts (the link key is
-- issuer and subject, never the email), single-use authorization states,
-- and first sign-ins waiting for the person to confirm a new account.

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
