-- SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- Repositories, rename redirects, collaborators, and the derived ref
-- snapshot with its push history (rebuildable from Git).

UPDATE schema_info SET version = 6;

CREATE TABLE repositories (
    id uuid PRIMARY KEY,
    namespace_id uuid NOT NULL REFERENCES namespaces (id),
    name citext NOT NULL,
    description text NULL CHECK (length(description) <= 500),
    visibility text NOT NULL CHECK (visibility IN ('public', 'private')),
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
