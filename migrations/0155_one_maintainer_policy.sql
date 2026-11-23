-- SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- The one-maintainer policy: a protection setting under which the only
-- person with write access can review their own merge requests, recorded
-- as a review of its own kind (never an approval), and the ledger entry of
-- a merge that relied on it.

ALTER TABLE branch_protections ADD COLUMN single_maintainer bool NOT NULL DEFAULT false;

ALTER TABLE mr_reviews DROP CONSTRAINT mr_reviews_verdict_check;
ALTER TABLE mr_reviews ADD CONSTRAINT mr_reviews_verdict_check
    CHECK (verdict IN ('approve', 'request_changes', 'comment', 'accountable_review'));

ALTER TABLE mr_ledger DROP CONSTRAINT mr_ledger_kind_check;
ALTER TABLE mr_ledger ADD CONSTRAINT mr_ledger_kind_check
    CHECK (kind IN ('binding', 'approval_carried', 'approval_stale',
        'status_stale', 'target_moved', 'signature_removed', 'one_maintainer_policy'));

UPDATE schema_info SET version = 155;
