-- SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- Agent context records: an agent's account of how it made a change,
-- uploaded through the API next to the change. The metadata is here; the
-- payload (summary, transcript, artifact digests, redaction counts) is a
-- blob encrypted with a key of its own, and that key is sealed with the
-- instance master key in `payload_key_enc`, so key rotation re-encrypts
-- only the key. Nothing of a record is ever in a repository's Git objects.

ALTER TABLE repositories
    ADD COLUMN context_detail text NOT NULL DEFAULT 'summary'
        CHECK (context_detail IN ('summary', 'full')),
    ADD COLUMN context_entire_import bool NOT NULL DEFAULT false;

CREATE TABLE context_records (
    id uuid PRIMARY KEY,
    repo_id uuid NOT NULL REFERENCES repositories (id) ON DELETE CASCADE,
    uploader_id uuid NULL REFERENCES users (id) ON DELETE SET NULL,
    initiated_by uuid NULL REFERENCES users (id) ON DELETE SET NULL,
    agent_name text NOT NULL CHECK (length(agent_name) BETWEEN 1 AND 200),
    agent_model text NULL CHECK (length(agent_model) <= 200),
    agent_tool text NULL CHECK (length(agent_tool) <= 200),
    -- The repository's detail level when it was uploaded.
    detail text NOT NULL CHECK (detail IN ('summary', 'full')),
    -- What the uploader asked to bind to (merge request number, change
    -- display ID, commit), kept to explain an unlinked record.
    requested_mr int NULL,
    requested_change text NULL CHECK (requested_change ~ '^[k-z]{32}$'),
    requested_commit text NULL CHECK (requested_commit ~ '^[0-9a-f]{40}$'),
    -- The binding Slath made: a merge request, and within it a change.
    mr_id uuid NULL REFERENCES merge_requests (id) ON DELETE SET NULL,
    change_id uuid NULL REFERENCES mr_changes (id) ON DELETE SET NULL,
    -- none: nothing asked; bound; unlinked: asked, but the uploader can't
    -- update that merge request or it wasn't found.
    binding_state text NOT NULL CHECK (binding_state IN ('none', 'bound', 'unlinked')),
    payload_sha256 text NOT NULL REFERENCES blobs (sha256),
    payload_bytes int NOT NULL CHECK (payload_bytes >= 0),
    transcript_entries int NOT NULL DEFAULT 0 CHECK (transcript_entries >= 0),
    payload_key_enc bytea NOT NULL,
    key_id text NOT NULL REFERENCES encryption_keys (key_id),
    created_at timestamptz NOT NULL DEFAULT now(),
    CHECK (change_id IS NULL OR mr_id IS NOT NULL),
    CHECK ((binding_state = 'bound') = (mr_id IS NOT NULL))
);
CREATE INDEX context_records_repo ON context_records (repo_id, created_at DESC, id DESC);
CREATE INDEX context_records_mr ON context_records (mr_id) WHERE mr_id IS NOT NULL;
CREATE INDEX context_records_change ON context_records (change_id) WHERE change_id IS NOT NULL;
CREATE INDEX context_records_key_id ON context_records (key_id);

CREATE FUNCTION slath_context_blob_ref() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP IN ('UPDATE', 'DELETE')
       AND (TG_OP = 'DELETE' OR OLD.payload_sha256 IS DISTINCT FROM NEW.payload_sha256) THEN
        PERFORM slath_blob_ref(OLD.payload_sha256, -1);
    END IF;
    IF TG_OP IN ('UPDATE', 'INSERT')
       AND (TG_OP = 'INSERT' OR OLD.payload_sha256 IS DISTINCT FROM NEW.payload_sha256) THEN
        PERFORM slath_blob_ref(NEW.payload_sha256, 1);
    END IF;
    RETURN NULL;
END;
$$;

CREATE TRIGGER context_records_blob_ref AFTER INSERT OR UPDATE OF payload_sha256 OR DELETE ON context_records
    FOR EACH ROW EXECUTE FUNCTION slath_context_blob_ref();

CREATE OR REPLACE FUNCTION slath_blob_recount() RETURNS void
LANGUAGE sql AS $$
    WITH uses AS (
        SELECT blob_sha256 AS sha, count(*) AS n FROM attachments GROUP BY blob_sha256
        UNION ALL
        SELECT avatar_blob_sha256, count(*) FROM users WHERE avatar_blob_sha256 IS NOT NULL GROUP BY 1
        UNION ALL
        SELECT avatar_blob_sha256, count(*) FROM organizations WHERE avatar_blob_sha256 IS NOT NULL GROUP BY 1
        UNION ALL
        SELECT blob_sha256, count(*) FROM instance_logos GROUP BY 1
        UNION ALL
        SELECT payload_sha256, count(*) FROM context_records GROUP BY 1
    ), total AS (SELECT sha, sum(n)::int AS n FROM uses GROUP BY sha)
    UPDATE blobs b
       SET refcount = COALESCE(t.n, 0),
           unreferenced_since = CASE WHEN COALESCE(t.n, 0) = 0
                                     THEN COALESCE(b.unreferenced_since, now()) END
      FROM blobs b2 LEFT JOIN total t ON t.sha = b2.sha256
     WHERE b.sha256 = b2.sha256;
$$;

UPDATE schema_info SET version = 156;
