-- SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- Notifications: subscriptions (watching or ignoring a repository or a
-- thread), one inbox row per user and thread, and the email preference.
-- Repository owners watch the repositories they created.

UPDATE schema_info SET version = 20;

ALTER TABLE users ADD COLUMN email_notifications bool NOT NULL DEFAULT true;

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

INSERT INTO subscriptions (user_id, subject_kind, subject_id, repo_id, state)
SELECT u.id, 'repo', r.id, r.id, 'watching'
  FROM repositories r JOIN users u ON u.namespace_id = r.namespace_id
ON CONFLICT DO NOTHING;
