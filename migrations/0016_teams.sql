-- SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- Teams with repository access, the organization MFA policy, and one SQL
-- function that decides whether a user can read a repository through a
-- grant. List and search queries filter with it, so visibility rules live
-- in one place (crates/core/src/authz.rs `effective_role` is the same
-- rule for one repository; tests compare them).

UPDATE schema_info SET version = 16;

ALTER TABLE organizations ADD COLUMN require_mfa bool NOT NULL DEFAULT false;

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
