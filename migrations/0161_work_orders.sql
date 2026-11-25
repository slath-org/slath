-- SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- Work orders: a person with write access delegates an issue, or a
-- follow-up on a merge request, to a bot. The bot claims the order for a
-- short-lived task token that may push only branches under `wo/<n>/`.
-- Orders have their own numbers per repository, never reused. Each
-- decision about something a task token tried is an order event, kept as
-- long as the repository (the audit log's retention doesn't prune them).

ALTER TABLE repositories
    ADD COLUMN next_work_order_number int NOT NULL DEFAULT 1;

-- A bot whose own tokens and SSH keys may only read and claim orders.
-- Every existing row is false, so the check needs no scan (NOT VALID).
ALTER TABLE users
    ADD COLUMN work_orders_only boolean NOT NULL DEFAULT false,
    ADD CONSTRAINT users_work_orders_only_bot CHECK (NOT work_orders_only OR kind = 'bot') NOT VALID;

CREATE TABLE work_orders (
    id uuid PRIMARY KEY,
    repo_id uuid NOT NULL REFERENCES repositories (id) ON DELETE CASCADE,
    number int NOT NULL CHECK (number > 0),
    -- The target: an issue or a merge request of the repository.
    issue_id uuid NULL REFERENCES issues (id) ON DELETE SET NULL,
    mr_id uuid NULL REFERENCES merge_requests (id) ON DELETE SET NULL,
    target_kind text NOT NULL CHECK (target_kind IN ('issue', 'merge_request')),
    bot_id uuid NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    creator_id uuid NULL REFERENCES users (id) ON DELETE SET NULL,
    origin text NOT NULL
        CHECK (origin IN ('writer', 'member', 'bot', 'outside_contributor', 'anonymous')),
    goal text NOT NULL CHECK (length(goal) BETWEEN 1 AND 16384),
    instructions text NULL CHECK (length(instructions) BETWEEN 1 AND 65536),
    scope text[] NOT NULL DEFAULT '{}' CHECK (cardinality(scope) <= 50),
    max_revisions int NOT NULL CHECK (max_revisions > 0),
    max_open_mrs int NOT NULL CHECK (max_open_mrs BETWEEN 1 AND 10),
    expires_at timestamptz NOT NULL,
    required_checks text[] NOT NULL DEFAULT '{}' CHECK (cardinality(required_checks) <= 20),
    reviewer_ids uuid[] NOT NULL DEFAULT '{}' CHECK (cardinality(reviewer_ids) <= 10),
    state text NOT NULL DEFAULT 'open' CHECK (state IN ('open', 'ended')),
    end_reason text NULL CHECK (end_reason IN ('merged', 'closed', 'expired', 'stopped')),
    ended_by uuid NULL REFERENCES users (id) ON DELETE SET NULL,
    ended_at timestamptz NULL,
    claimed_at timestamptz NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (repo_id, number),
    CHECK ((state = 'ended') = (end_reason IS NOT NULL AND ended_at IS NOT NULL)),
    CHECK ((target_kind = 'issue') = (mr_id IS NULL))
);
-- At most one open order per target.
CREATE UNIQUE INDEX work_orders_open_issue ON work_orders (issue_id)
    WHERE state = 'open' AND issue_id IS NOT NULL;
CREATE UNIQUE INDEX work_orders_open_mr ON work_orders (mr_id)
    WHERE state = 'open' AND mr_id IS NOT NULL;
CREATE INDEX work_orders_bot_open ON work_orders (bot_id) WHERE state = 'open';
CREATE INDEX work_orders_expiry ON work_orders (expires_at) WHERE state = 'open';

-- Task tokens: the credential a claim returns. A claim revokes the
-- order's earlier ones; they also stop working when the order ends.
CREATE TABLE task_tokens (
    id uuid PRIMARY KEY,
    order_id uuid NOT NULL REFERENCES work_orders (id) ON DELETE CASCADE,
    token_prefix text NOT NULL CHECK (length(token_prefix) = 8),
    token_hash bytea NOT NULL UNIQUE CHECK (length(token_hash) = 32),
    expires_at timestamptz NOT NULL,
    revoked_at timestamptz NULL,
    last_used_at timestamptz NULL,
    created_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX task_tokens_order ON task_tokens (order_id);

-- What happened under an order, with the decision and the policy that
-- made it: the order's evidence.
CREATE TABLE work_order_events (
    id uuid PRIMARY KEY,
    order_id uuid NOT NULL REFERENCES work_orders (id) ON DELETE CASCADE,
    action text NOT NULL CHECK (length(action) BETWEEN 1 AND 64),
    decision text NOT NULL CHECK (decision IN ('allowed', 'denied')),
    policy text NULL CHECK (length(policy) BETWEEN 1 AND 64),
    -- A ref, merge request, issue, or commit, as text.
    target text NULL CHECK (length(target) <= 1024),
    actor_id uuid NULL REFERENCES users (id) ON DELETE SET NULL,
    created_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX work_order_events_order ON work_order_events (order_id, created_at, id);

UPDATE schema_info SET version = 161;
