-- SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- The instance logo administrators upload, one per theme. Without a row,
-- pages show the bundled Slath logo for that theme. The image is a blob
-- (a checked SVG or a re-encoded PNG), counted like avatars.

CREATE TABLE instance_logos (
    theme text PRIMARY KEY CHECK (theme IN ('light', 'dark')),
    blob_sha256 text NOT NULL REFERENCES blobs (sha256),
    content_type text NOT NULL CHECK (content_type IN ('image/svg+xml', 'image/png')),
    updated_by uuid NULL REFERENCES users (id) ON DELETE SET NULL,
    updated_at timestamptz NOT NULL DEFAULT now()
);

CREATE FUNCTION slath_logo_blob_ref() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP IN ('UPDATE', 'DELETE')
       AND (TG_OP = 'DELETE' OR OLD.blob_sha256 IS DISTINCT FROM NEW.blob_sha256) THEN
        PERFORM slath_blob_ref(OLD.blob_sha256, -1);
    END IF;
    IF TG_OP IN ('UPDATE', 'INSERT')
       AND (TG_OP = 'INSERT' OR OLD.blob_sha256 IS DISTINCT FROM NEW.blob_sha256) THEN
        PERFORM slath_blob_ref(NEW.blob_sha256, 1);
    END IF;
    RETURN NULL;
END;
$$;

CREATE TRIGGER instance_logos_blob_ref AFTER INSERT OR UPDATE OF blob_sha256 OR DELETE ON instance_logos
    FOR EACH ROW EXECUTE FUNCTION slath_logo_blob_ref();

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
    ), total AS (SELECT sha, sum(n)::int AS n FROM uses GROUP BY sha)
    UPDATE blobs b
       SET refcount = COALESCE(t.n, 0),
           unreferenced_since = CASE WHEN COALESCE(t.n, 0) = 0
                                     THEN COALESCE(b.unreferenced_since, now()) END
      FROM blobs b2 LEFT JOIN total t ON t.sha = b2.sha256
     WHERE b.sha256 = b2.sha256;
$$;

UPDATE schema_info SET version = 139;
