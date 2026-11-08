-- SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- A stricter rule for branch protection: any new revision makes earlier
-- approvals stale, even a rebase that keeps every patch.

UPDATE schema_info SET version = 111;

ALTER TABLE branch_protections ADD COLUMN dismiss_approvals_on_new_revision bool NOT NULL DEFAULT false;
