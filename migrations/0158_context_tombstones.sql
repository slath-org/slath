-- SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- Deleting or expiring an agent context record leaves a tombstone: the
-- same row with everything the agent claimed removed (its name, model,
-- and tool) and the payload gone (its sealed key cleared, its blob
-- released). What stays says that a record existed, what it was about,
-- who uploaded it and for whom, and who deleted it or that retention did.
-- Because it is the same row, the same people see it as saw the record.

ALTER TABLE context_records
    ALTER COLUMN agent_name DROP NOT NULL,
    ALTER COLUMN payload_sha256 DROP NOT NULL,
    ALTER COLUMN payload_key_enc DROP NOT NULL,
    ALTER COLUMN key_id DROP NOT NULL,
    ADD COLUMN deleted_at timestamptz NULL,
    -- The person who deleted it; NULL with `deleted_by_retention` or once
    -- that account is gone.
    ADD COLUMN deleted_by uuid NULL REFERENCES users (id) ON DELETE SET NULL,
    ADD COLUMN deleted_by_retention bool NOT NULL DEFAULT false,
    -- The payload's digest as stored, kept once the blob is released.
    ADD COLUMN deleted_payload_sha256 text NULL
        CHECK (deleted_payload_sha256 ~ '^[0-9a-f]{64}$'),
    ADD CONSTRAINT context_records_tombstone CHECK (
        CASE WHEN deleted_at IS NULL
             THEN agent_name IS NOT NULL AND payload_sha256 IS NOT NULL
                  AND payload_key_enc IS NOT NULL AND key_id IS NOT NULL
                  AND deleted_by IS NULL AND NOT deleted_by_retention
                  AND deleted_payload_sha256 IS NULL
             ELSE agent_name IS NULL AND agent_model IS NULL AND agent_tool IS NULL
                  AND payload_sha256 IS NULL AND payload_key_enc IS NULL AND key_id IS NULL
                  AND deleted_payload_sha256 IS NOT NULL
        END);

UPDATE schema_info SET version = 158;
