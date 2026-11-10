-- SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- Deploy keys: SSH keys of a repository instead of a user. They share the
-- table with user keys, so a fingerprint still belongs to exactly one
-- principal. A deploy key reads its repository and, with can_push, pushes.

ALTER TABLE ssh_keys
    ALTER COLUMN user_id DROP NOT NULL,
    ADD COLUMN repo_id uuid NULL REFERENCES repositories (id) ON DELETE CASCADE,
    ADD COLUMN can_push bool NOT NULL DEFAULT false,
    ADD COLUMN added_by uuid NULL REFERENCES users (id) ON DELETE SET NULL,
    ADD CONSTRAINT ssh_keys_one_owner CHECK ((user_id IS NULL) <> (repo_id IS NULL)),
    ADD CONSTRAINT ssh_keys_push_for_deploy_keys CHECK (repo_id IS NOT NULL OR NOT can_push);

CREATE INDEX ssh_keys_repo_id_idx ON ssh_keys (repo_id) WHERE repo_id IS NOT NULL;

UPDATE schema_info SET version = 120;
