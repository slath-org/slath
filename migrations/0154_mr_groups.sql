-- SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- Linked merge requests: an ordered group of merge requests, in one or
-- several repositories, that merge together. The group's state (open,
-- partially landed, merged) follows from its members; it only remembers
-- why its last group merge stopped and whether one is running.

CREATE TABLE mr_groups (
    id uuid PRIMARY KEY,
    created_by uuid NULL REFERENCES users (id) ON DELETE SET NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    -- {mr_id, reason, at, actor}: why the last group merge stopped.
    last_failure jsonb NULL,
    -- Set while a group merge runs; older claims count as abandoned.
    merging_since timestamptz NULL
);

CREATE TABLE mr_group_members (
    group_id uuid NOT NULL REFERENCES mr_groups (id) ON DELETE CASCADE,
    mr_id uuid NOT NULL UNIQUE REFERENCES merge_requests (id) ON DELETE CASCADE,
    position integer NOT NULL CHECK (position >= 0),
    PRIMARY KEY (group_id, mr_id),
    UNIQUE (group_id, position) DEFERRABLE INITIALLY DEFERRED
);

UPDATE schema_info SET version = 154;
