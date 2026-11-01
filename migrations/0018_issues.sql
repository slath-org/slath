-- SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- Issues with comments, timeline, labels, milestones, and assignees.
-- Issue numbers come from repositories.next_item_number, shared with merge
-- requests, in the transaction that inserts the issue.

UPDATE schema_info SET version = 18;

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
    UNIQUE (repo_id, number)
);
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
    created_at timestamptz NOT NULL DEFAULT now()
);
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
