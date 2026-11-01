-- SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- Organizations and their members. Organizations share the namespace table
-- with users, so names never collide (case-insensitively).

UPDATE schema_info SET version = 5;

CREATE TABLE organizations (
    id uuid PRIMARY KEY,
    namespace_id uuid NOT NULL UNIQUE REFERENCES namespaces (id),
    display_name text NULL CHECK (length(display_name) <= 100),
    description text NULL CHECK (length(description) <= 500),
    default_member_role text NOT NULL DEFAULT 'none' CHECK (default_member_role IN ('none', 'read')),
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE org_members (
    org_id uuid NOT NULL REFERENCES organizations (id) ON DELETE CASCADE,
    user_id uuid NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    role text NOT NULL CHECK (role IN ('owner', 'member')),
    created_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (org_id, user_id)
);
CREATE INDEX org_members_user_id_idx ON org_members (user_id);
