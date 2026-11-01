-- SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- Full-text search over issues (title and description) and comments with
-- the language-neutral 'simple' configuration, and trigram indexes for
-- repository names and descriptions.

UPDATE schema_info SET version = 21;

CREATE EXTENSION IF NOT EXISTS pg_trgm;

ALTER TABLE issues ADD COLUMN search tsvector GENERATED ALWAYS AS (
    setweight(to_tsvector('simple', title), 'A') || setweight(to_tsvector('simple', body), 'B')
) STORED;
CREATE INDEX issues_search ON issues USING gin (search);

ALTER TABLE comments ADD COLUMN search tsvector GENERATED ALWAYS AS (
    to_tsvector('simple', body)
) STORED;
CREATE INDEX comments_search ON comments USING gin (search);

CREATE INDEX repositories_name_trgm ON repositories USING gin ((name::text) gin_trgm_ops);
CREATE INDEX repositories_description_trgm ON repositories USING gin (description gin_trgm_ops);
