-- SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- A derived snapshot of each merge request's readiness (its readiness
-- card's tone and headline) for lists. Never trusted for merging.
-- `generation` counts the changes that can affect readiness; triggers bump
-- it, so no code path can forget to. A snapshot is current while
-- built_generation = generation, and is stored only if generation didn't
-- move while it was computed.

CREATE TABLE mr_readiness (
    mr_id uuid PRIMARY KEY REFERENCES merge_requests (id) ON DELETE CASCADE,
    generation bigint NOT NULL DEFAULT 0,
    built_generation bigint NULL,
    tone text NULL CHECK (tone IN ('done', 'waiting', 'blocking')),
    headline text NULL CHECK (length(headline) <= 300),
    built_at timestamptz NULL
);

-- For statuses: the revisions with a given head.
CREATE INDEX mr_revisions_head ON mr_revisions (head_oid);

-- Bumps the merge requests that still exist (a cascading delete removes
-- the merge request before its rows, so they're skipped).
CREATE FUNCTION slath_readiness_bump(ids uuid[]) RETURNS void
LANGUAGE sql AS $$
    INSERT INTO mr_readiness (mr_id, generation)
    SELECT m.id, 1 FROM merge_requests m WHERE m.id = ANY (ids)
    ON CONFLICT (mr_id) DO UPDATE SET generation = mr_readiness.generation + 1
$$;

-- Rows naming their merge request in column TG_ARGV[0].
CREATE FUNCTION slath_readiness_by_mr() RETURNS trigger
LANGUAGE plpgsql AS $$
DECLARE ids uuid[] := '{}';
BEGIN
    IF TG_OP <> 'INSERT' THEN ids := ids || (to_jsonb(OLD) ->> TG_ARGV[0])::uuid; END IF;
    IF TG_OP <> 'DELETE' THEN ids := ids || (to_jsonb(NEW) ->> TG_ARGV[0])::uuid; END IF;
    PERFORM slath_readiness_bump(ids);
    RETURN NULL;
END;
$$;

-- Rows naming a revision (TG_ARGV[0]) or a thread (TG_ARGV[1]).
CREATE FUNCTION slath_readiness_by_parent() RETURNS trigger
LANGUAGE plpgsql AS $$
DECLARE
    row jsonb := COALESCE(to_jsonb(NEW), to_jsonb(OLD));
BEGIN
    IF TG_ARGV[0] = 'revision' THEN
        PERFORM slath_readiness_bump(ARRAY(
            SELECT r.mr_id FROM mr_revisions r WHERE r.id = (row ->> 'revision_id')::uuid));
    ELSE
        PERFORM slath_readiness_bump(ARRAY(
            SELECT t.mr_id FROM mr_threads t WHERE t.id = (row ->> 'thread_id')::uuid));
    END IF;
    RETURN NULL;
END;
$$;

-- Rows naming a repository in column TG_ARGV[0]: its open merge requests.
CREATE FUNCTION slath_readiness_by_repo() RETURNS trigger
LANGUAGE plpgsql AS $$
DECLARE
    row jsonb := COALESCE(to_jsonb(NEW), to_jsonb(OLD));
BEGIN
    PERFORM slath_readiness_bump(ARRAY(
        SELECT m.id FROM merge_requests m
        WHERE m.repo_id = (row ->> TG_ARGV[0])::uuid AND m.state = 'open'));
    RETURN NULL;
END;
$$;

-- A status: the open merge requests with a revision whose head it is.
CREATE FUNCTION slath_readiness_by_status() RETURNS trigger
LANGUAGE plpgsql AS $$
DECLARE
    row jsonb := COALESCE(to_jsonb(NEW), to_jsonb(OLD));
BEGIN
    PERFORM slath_readiness_bump(ARRAY(
        SELECT m.id FROM merge_requests m JOIN mr_revisions r ON r.mr_id = m.id
        WHERE m.repo_id = (row ->> 'repo_id')::uuid AND r.head_oid = row ->> 'commit_oid'
          AND m.state = 'open'));
    RETURN NULL;
END;
$$;

-- A ref update: the open merge requests into that branch.
CREATE FUNCTION slath_readiness_by_ref() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    PERFORM slath_readiness_bump(ARRAY(
        SELECT m.id FROM merge_requests m JOIN push_events p ON p.repo_id = m.repo_id
        WHERE p.id = NEW.push_id AND m.state = 'open'
          AND convert_to('refs/heads/' || m.target_branch, 'UTF8') = NEW.name));
    RETURN NULL;
END;
$$;

-- Memberships that can change who counts as an approver anywhere: every
-- snapshot (rare; they rebuild when shown).
CREATE FUNCTION slath_readiness_all() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    UPDATE mr_readiness SET generation = generation + 1;
    RETURN NULL;
END;
$$;

CREATE TRIGGER merge_requests_readiness AFTER INSERT OR UPDATE ON merge_requests
    FOR EACH ROW EXECUTE FUNCTION slath_readiness_by_mr('id');
CREATE TRIGGER mr_revisions_readiness AFTER INSERT OR UPDATE OR DELETE ON mr_revisions
    FOR EACH ROW EXECUTE FUNCTION slath_readiness_by_mr('mr_id');
CREATE TRIGGER mr_changes_readiness AFTER INSERT OR UPDATE OR DELETE ON mr_changes
    FOR EACH ROW EXECUTE FUNCTION slath_readiness_by_mr('mr_id');
CREATE TRIGGER mr_reviews_readiness AFTER INSERT OR UPDATE OR DELETE ON mr_reviews
    FOR EACH ROW EXECUTE FUNCTION slath_readiness_by_mr('mr_id');
CREATE TRIGGER mr_threads_readiness AFTER INSERT OR UPDATE OR DELETE ON mr_threads
    FOR EACH ROW EXECUTE FUNCTION slath_readiness_by_mr('mr_id');
CREATE TRIGGER mr_change_versions_readiness AFTER INSERT OR UPDATE OR DELETE ON mr_change_versions
    FOR EACH ROW EXECUTE FUNCTION slath_readiness_by_parent('revision');
CREATE TRIGGER mr_thread_comments_readiness AFTER INSERT OR UPDATE OR DELETE ON mr_thread_comments
    FOR EACH ROW EXECUTE FUNCTION slath_readiness_by_parent('thread');
CREATE TRIGGER commit_statuses_readiness AFTER INSERT OR UPDATE OR DELETE ON commit_statuses
    FOR EACH ROW EXECUTE FUNCTION slath_readiness_by_status();
CREATE TRIGGER ref_updates_readiness AFTER INSERT ON ref_updates
    FOR EACH ROW EXECUTE FUNCTION slath_readiness_by_ref();
CREATE TRIGGER branch_protections_readiness AFTER INSERT OR UPDATE OR DELETE ON branch_protections
    FOR EACH ROW EXECUTE FUNCTION slath_readiness_by_repo('repo_id');
CREATE TRIGGER repositories_readiness AFTER UPDATE OF sensitive_paths ON repositories
    FOR EACH ROW EXECUTE FUNCTION slath_readiness_by_repo('id');
CREATE TRIGGER repo_collaborators_readiness AFTER INSERT OR UPDATE OR DELETE ON repo_collaborators
    FOR EACH ROW EXECUTE FUNCTION slath_readiness_by_repo('repo_id');
CREATE TRIGGER repo_team_access_readiness AFTER INSERT OR UPDATE OR DELETE ON repo_team_access
    FOR EACH ROW EXECUTE FUNCTION slath_readiness_by_repo('repo_id');
CREATE TRIGGER team_members_readiness AFTER INSERT OR UPDATE OR DELETE ON team_members
    FOR EACH STATEMENT EXECUTE FUNCTION slath_readiness_all();
CREATE TRIGGER org_members_readiness AFTER INSERT OR UPDATE OR DELETE ON org_members
    FOR EACH STATEMENT EXECUTE FUNCTION slath_readiness_all();

UPDATE schema_info SET version = 144;
