-- SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- The review continuity ledger: an append-only record per merge request of
-- every identity decision and every effect on review state.

UPDATE schema_info SET version = 110;

CREATE TABLE mr_ledger (
    id uuid PRIMARY KEY,
    mr_id uuid NOT NULL REFERENCES merge_requests (id) ON DELETE CASCADE,
    revision_seq int NULL,
    kind text NOT NULL CHECK (kind IN ('binding', 'approval_carried', 'approval_stale',
        'status_stale', 'target_moved', 'signature_removed')),
    -- The change's display ID at the time (changes can be joined away).
    change_id text NULL CHECK (change_id ~ '^[k-z]{32}$'),
    state text NULL,
    reason text NULL,
    detail jsonb NOT NULL DEFAULT '{}',
    actor_id uuid NULL REFERENCES users (id) ON DELETE SET NULL,
    created_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX mr_ledger_mr ON mr_ledger (mr_id, created_at, id);
