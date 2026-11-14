-- SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- Origin labels: who wrote each piece of user-generated text, relative to
-- its repository and as they stood when writing it (writer, member, bot,
-- outside contributor, or anonymous). Edits by someone else keep the less
-- trusted of the two.

-- Least trusted first.
CREATE FUNCTION slath_origin_rank(o text) RETURNS int
LANGUAGE sql IMMUTABLE AS $$
    SELECT CASE o
        WHEN 'anonymous' THEN 0
        WHEN 'outside_contributor' THEN 1
        WHEN 'bot' THEN 2
        WHEN 'member' THEN 3
        WHEN 'writer' THEN 4
    END
$$;

-- The less trusted of two origins.
CREATE FUNCTION slath_origin_least(a text, b text) RETURNS text
LANGUAGE sql IMMUTABLE AS $$
    SELECT CASE WHEN slath_origin_rank(a) <= slath_origin_rank(b) THEN a ELSE b END
$$;

-- An existing row's origin, from the author's grants now (for rows written
-- before origins were recorded; new text gets its origin from the server).
CREATE FUNCTION pg_temp.backfill_origin(repo uuid, author uuid) RETURNS text
LANGUAGE sql STABLE AS $$
    SELECT CASE
        WHEN author IS NULL OR u.id IS NULL THEN 'anonymous'
        WHEN u.kind = 'bot' THEN 'bot'
        WHEN u.is_admin OR slath_owns_namespace(r.namespace_id, author)
            OR EXISTS (SELECT 1 FROM repo_collaborators c
                       WHERE c.repo_id = repo AND c.user_id = author AND c.role <> 'read')
            OR EXISTS (SELECT 1 FROM repo_team_access t JOIN team_members m ON m.team_id = t.team_id
                       WHERE t.repo_id = repo AND m.user_id = author AND t.role <> 'read')
            OR EXISTS (SELECT 1 FROM organizations o JOIN org_members m ON m.org_id = o.id
                       WHERE o.namespace_id = r.namespace_id AND m.user_id = author AND m.role = 'owner')
            THEN 'writer'
        WHEN EXISTS (SELECT 1 FROM repo_collaborators c WHERE c.repo_id = repo AND c.user_id = author)
            OR EXISTS (SELECT 1 FROM repo_team_access t JOIN team_members m ON m.team_id = t.team_id
                       WHERE t.repo_id = repo AND m.user_id = author)
            OR EXISTS (SELECT 1 FROM organizations o JOIN org_members m ON m.org_id = o.id
                       WHERE o.namespace_id = r.namespace_id AND m.user_id = author)
            THEN 'member'
        ELSE 'outside_contributor'
    END
    FROM repositories r LEFT JOIN users u ON u.id = author
    WHERE r.id = repo
$$;

ALTER TABLE issues ADD COLUMN origin text NULL;
UPDATE issues SET origin = pg_temp.backfill_origin(repo_id, author_id);
ALTER TABLE merge_requests ADD COLUMN origin text NULL;
UPDATE merge_requests SET origin = pg_temp.backfill_origin(repo_id, author_id);
ALTER TABLE comments ADD COLUMN origin text NULL;
UPDATE comments SET origin = pg_temp.backfill_origin(repo_id, author_id);
ALTER TABLE mr_thread_comments ADD COLUMN origin text NULL;
UPDATE mr_thread_comments c SET origin = pg_temp.backfill_origin(m.repo_id, c.author_id)
    FROM mr_threads t JOIN merge_requests m ON m.id = t.mr_id WHERE t.id = c.thread_id;
ALTER TABLE mr_reviews ADD COLUMN origin text NULL;
UPDATE mr_reviews v SET origin = pg_temp.backfill_origin(m.repo_id, v.reviewer_id)
    FROM merge_requests m WHERE m.id = v.mr_id;
ALTER TABLE commit_statuses ADD COLUMN origin text NULL;
UPDATE commit_statuses SET origin = CASE creator_kind
    WHEN 'ci' THEN 'bot' ELSE pg_temp.backfill_origin(repo_id, creator_id) END;
-- Labels, milestones, and descriptions have no recorded author; only
-- people with write access could write them so far.
ALTER TABLE labels ADD COLUMN origin text NULL;
UPDATE labels SET origin = 'writer';
ALTER TABLE milestones ADD COLUMN origin text NULL;
UPDATE milestones SET origin = 'writer';
ALTER TABLE repositories ADD COLUMN description_origin text NULL;
UPDATE repositories SET description_origin = 'writer';

-- From now on the server sets every origin (no default).
ALTER TABLE issues ALTER COLUMN origin SET NOT NULL,
    ADD CHECK (slath_origin_rank(origin) IS NOT NULL);
ALTER TABLE merge_requests ALTER COLUMN origin SET NOT NULL,
    ADD CHECK (slath_origin_rank(origin) IS NOT NULL);
ALTER TABLE comments ALTER COLUMN origin SET NOT NULL,
    ADD CHECK (slath_origin_rank(origin) IS NOT NULL);
ALTER TABLE mr_thread_comments ALTER COLUMN origin SET NOT NULL,
    ADD CHECK (slath_origin_rank(origin) IS NOT NULL);
ALTER TABLE mr_reviews ALTER COLUMN origin SET NOT NULL,
    ADD CHECK (slath_origin_rank(origin) IS NOT NULL);
ALTER TABLE commit_statuses ALTER COLUMN origin SET NOT NULL,
    ADD CHECK (slath_origin_rank(origin) IS NOT NULL);
ALTER TABLE labels ALTER COLUMN origin SET NOT NULL,
    ADD CHECK (slath_origin_rank(origin) IS NOT NULL);
ALTER TABLE milestones ALTER COLUMN origin SET NOT NULL,
    ADD CHECK (slath_origin_rank(origin) IS NOT NULL);
ALTER TABLE repositories ALTER COLUMN description_origin SET NOT NULL,
    ADD CHECK (slath_origin_rank(description_origin) IS NOT NULL);

UPDATE schema_info SET version = 134;
