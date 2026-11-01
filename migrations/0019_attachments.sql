-- SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- Content-addressed blobs (files under <data_dir>/blobs/), issue
-- attachments, and uploaded avatars. A blob's refcount counts the rows that
-- use it and is kept by triggers, so every way a row disappears (including
-- cascades from a deleted repository) releases its blob. Blobs at refcount
-- 0 are removed by the collector after a grace period.

UPDATE schema_info SET version = 19;

CREATE TABLE blobs (
    sha256 text PRIMARY KEY CHECK (sha256 ~ '^[0-9a-f]{64}$'),
    size_bytes bigint NOT NULL CHECK (size_bytes >= 0),
    refcount int NOT NULL DEFAULT 0 CHECK (refcount >= 0),
    unreferenced_since timestamptz NULL DEFAULT now(),
    created_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX blobs_unreferenced ON blobs (unreferenced_since) WHERE refcount = 0;

CREATE TABLE attachments (
    id uuid PRIMARY KEY,
    repo_id uuid NOT NULL REFERENCES repositories (id) ON DELETE CASCADE,
    uploader_id uuid NULL REFERENCES users (id) ON DELETE SET NULL,
    blob_sha256 text NOT NULL REFERENCES blobs (sha256),
    filename text NOT NULL CHECK (length(filename) BETWEEN 1 AND 255),
    content_type text NOT NULL,
    size_bytes bigint NOT NULL CHECK (size_bytes >= 0),
    created_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX attachments_repo ON attachments (repo_id, created_at);
CREATE INDEX attachments_blob ON attachments (blob_sha256);

-- users.avatar_blob_sha256 exists since the first schema but was never set.
UPDATE users SET avatar_blob_sha256 = NULL WHERE avatar_blob_sha256 IS NOT NULL;
ALTER TABLE users ADD CONSTRAINT users_avatar_blob_fkey
    FOREIGN KEY (avatar_blob_sha256) REFERENCES blobs (sha256);
ALTER TABLE organizations ADD COLUMN avatar_blob_sha256 text NULL REFERENCES blobs (sha256);

CREATE FUNCTION slath_blob_ref(sha text, delta int) RETURNS void
LANGUAGE sql AS $$
    UPDATE blobs
       SET refcount = refcount + delta,
           unreferenced_since = CASE WHEN refcount + delta = 0 THEN now() ELSE NULL END
     WHERE sha256 = sha;
$$;

CREATE FUNCTION slath_attachment_blob_ref() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP = 'INSERT' THEN
        PERFORM slath_blob_ref(NEW.blob_sha256, 1);
    ELSIF TG_OP = 'DELETE' THEN
        PERFORM slath_blob_ref(OLD.blob_sha256, -1);
    END IF;
    RETURN NULL;
END;
$$;

CREATE TRIGGER attachments_blob_ref AFTER INSERT OR DELETE ON attachments
    FOR EACH ROW EXECUTE FUNCTION slath_attachment_blob_ref();

CREATE FUNCTION slath_avatar_blob_ref() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP IN ('UPDATE', 'DELETE') AND OLD.avatar_blob_sha256 IS NOT NULL
       AND (TG_OP = 'DELETE' OR OLD.avatar_blob_sha256 IS DISTINCT FROM NEW.avatar_blob_sha256) THEN
        PERFORM slath_blob_ref(OLD.avatar_blob_sha256, -1);
    END IF;
    IF TG_OP IN ('UPDATE', 'INSERT') AND NEW.avatar_blob_sha256 IS NOT NULL
       AND (TG_OP = 'INSERT' OR OLD.avatar_blob_sha256 IS DISTINCT FROM NEW.avatar_blob_sha256) THEN
        PERFORM slath_blob_ref(NEW.avatar_blob_sha256, 1);
    END IF;
    RETURN NULL;
END;
$$;

CREATE TRIGGER users_avatar_blob_ref AFTER INSERT OR UPDATE OF avatar_blob_sha256 OR DELETE ON users
    FOR EACH ROW EXECUTE FUNCTION slath_avatar_blob_ref();
CREATE TRIGGER organizations_avatar_blob_ref AFTER INSERT OR UPDATE OF avatar_blob_sha256 OR DELETE ON organizations
    FOR EACH ROW EXECUTE FUNCTION slath_avatar_blob_ref();

-- Recomputes every refcount from the referencing rows. A restore loads
-- tables with the triggers active, so it calls this afterwards.
CREATE FUNCTION slath_blob_recount() RETURNS void
LANGUAGE sql AS $$
    WITH uses AS (
        SELECT blob_sha256 AS sha, count(*) AS n FROM attachments GROUP BY blob_sha256
        UNION ALL
        SELECT avatar_blob_sha256, count(*) FROM users WHERE avatar_blob_sha256 IS NOT NULL GROUP BY 1
        UNION ALL
        SELECT avatar_blob_sha256, count(*) FROM organizations WHERE avatar_blob_sha256 IS NOT NULL GROUP BY 1
    ), total AS (SELECT sha, sum(n)::int AS n FROM uses GROUP BY sha)
    UPDATE blobs b
       SET refcount = COALESCE(t.n, 0),
           unreferenced_since = CASE WHEN COALESCE(t.n, 0) = 0
                                     THEN COALESCE(b.unreferenced_since, now()) END
      FROM blobs b2 LEFT JOIN total t ON t.sha = b2.sha256
     WHERE b.sha256 = b2.sha256;
$$;
