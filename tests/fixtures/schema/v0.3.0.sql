-- SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- Schema and data of Slath 0.3.0 (schema 139), written by `just schema-fixture`.
-- Users: alice, bob, root-admin (password "slath test password"). Don't edit.

--
-- PostgreSQL database dump
--

\restrict zGovOBUUgcU9BZAoWthlslC0y4gyHHbL9qwH2bM4dSts4J8eALgVO2u9bhRe27w

-- Dumped from database version 18.7 (Debian 18.7-1.pgdg13+2)
-- Dumped by pg_dump version 18.7 (Debian 18.7-1.pgdg13+2)

SET statement_timeout = 0;
SET lock_timeout = 0;
SET idle_in_transaction_session_timeout = 0;
SET transaction_timeout = 0;
SET client_encoding = 'UTF8';
SET standard_conforming_strings = on;
SELECT pg_catalog.set_config('search_path', '', false);
SET check_function_bodies = false;
SET xmloption = content;
SET client_min_messages = warning;
SET row_security = off;

--
-- Name: citext; Type: EXTENSION; Schema: -; Owner: -
--

CREATE EXTENSION IF NOT EXISTS citext WITH SCHEMA public;


--
-- Name: EXTENSION citext; Type: COMMENT; Schema: -; Owner: -
--

COMMENT ON EXTENSION citext IS 'data type for case-insensitive character strings';


--
-- Name: pg_trgm; Type: EXTENSION; Schema: -; Owner: -
--

CREATE EXTENSION IF NOT EXISTS pg_trgm WITH SCHEMA public;


--
-- Name: EXTENSION pg_trgm; Type: COMMENT; Schema: -; Owner: -
--

COMMENT ON EXTENSION pg_trgm IS 'text similarity measurement and index searching based on trigrams';


--
-- Name: audit_events_append_only(); Type: FUNCTION; Schema: public; Owner: -
--

CREATE FUNCTION public.audit_events_append_only() RETURNS trigger
    LANGUAGE plpgsql
    AS $$
BEGIN
    IF TG_OP = 'DELETE' AND current_setting('slath.audit_purge', true) = 'on' THEN
        RETURN OLD;
    END IF;
    RAISE EXCEPTION 'audit_events is append-only';
END;
$$;


--
-- Name: slath_attachment_blob_ref(); Type: FUNCTION; Schema: public; Owner: -
--

CREATE FUNCTION public.slath_attachment_blob_ref() RETURNS trigger
    LANGUAGE plpgsql
    AS $$
BEGIN
    IF TG_OP = 'INSERT' THEN
        PERFORM slath_blob_ref(NEW.blob_sha256, 1);
    ELSIF TG_OP = 'DELETE' THEN
        PERFORM slath_blob_ref(OLD.blob_sha256, -1);
    END IF;
    RETURN NULL;
END;
$$;


--
-- Name: slath_avatar_blob_ref(); Type: FUNCTION; Schema: public; Owner: -
--

CREATE FUNCTION public.slath_avatar_blob_ref() RETURNS trigger
    LANGUAGE plpgsql
    AS $$
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


--
-- Name: slath_blob_recount(); Type: FUNCTION; Schema: public; Owner: -
--

CREATE FUNCTION public.slath_blob_recount() RETURNS void
    LANGUAGE sql
    AS $$
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


--
-- Name: slath_blob_ref(text, integer); Type: FUNCTION; Schema: public; Owner: -
--

CREATE FUNCTION public.slath_blob_ref(sha text, delta integer) RETURNS void
    LANGUAGE sql
    AS $$
    UPDATE blobs
       SET refcount = refcount + delta,
           unreferenced_since = CASE WHEN refcount + delta = 0 THEN now() ELSE NULL END
     WHERE sha256 = sha;
$$;


--
-- Name: slath_bot_frozen(uuid); Type: FUNCTION; Schema: public; Owner: -
--

CREATE FUNCTION public.slath_bot_frozen(bot uuid) RETURNS boolean
    LANGUAGE sql STABLE
    AS $$
    SELECT EXISTS (
        SELECT 1 FROM users u JOIN bot_freezes f
            ON f.org_id IS NULL OR (u.bot_owner_kind = 'org' AND f.org_id = u.bot_owner_id)
        WHERE u.id = bot AND u.kind = 'bot'
    )
$$;


--
-- Name: slath_can_read(uuid, uuid); Type: FUNCTION; Schema: public; Owner: -
--

CREATE FUNCTION public.slath_can_read(repo uuid, viewer uuid) RETURNS boolean
    LANGUAGE sql STABLE
    AS $$
    SELECT slath_can_read_direct(repo, viewer) AND NOT EXISTS (
        SELECT 1 FROM users b JOIN users o ON o.id = b.bot_owner_id
        WHERE b.id = viewer AND b.kind = 'bot' AND b.bot_owner_kind = 'user'
          AND NOT (o.state = 'active' AND (o.is_admin OR slath_can_read_direct(repo, o.id)))
    )
$$;


--
-- Name: slath_can_read_direct(uuid, uuid); Type: FUNCTION; Schema: public; Owner: -
--

CREATE FUNCTION public.slath_can_read_direct(repo uuid, viewer uuid) RETURNS boolean
    LANGUAGE sql STABLE
    AS $$
    SELECT viewer IS NOT NULL AND EXISTS (
        SELECT 1 FROM repositories r
        LEFT JOIN organizations o ON o.namespace_id = r.namespace_id
        WHERE r.id = repo
          AND NOT (COALESCE(o.require_mfa, false) AND NOT slath_has_mfa(viewer))
          AND (
               EXISTS (SELECT 1 FROM repo_collaborators c WHERE c.repo_id = r.id AND c.user_id = viewer)
            OR slath_owns_namespace(r.namespace_id, viewer)
            OR EXISTS (SELECT 1 FROM org_members m WHERE m.org_id = o.id AND m.user_id = viewer
                         AND (m.role = 'owner' OR o.default_member_role <> 'none'))
            OR EXISTS (SELECT 1 FROM repo_team_access a JOIN team_members tm ON tm.team_id = a.team_id
                       WHERE a.repo_id = r.id AND tm.user_id = viewer)
          )
    )
$$;


--
-- Name: slath_has_mfa(uuid); Type: FUNCTION; Schema: public; Owner: -
--

CREATE FUNCTION public.slath_has_mfa(viewer uuid) RETURNS boolean
    LANGUAGE sql STABLE
    AS $$
    SELECT EXISTS (SELECT 1 FROM totp_credentials WHERE user_id = viewer AND confirmed)
        OR EXISTS (SELECT 1 FROM webauthn_credentials WHERE user_id = viewer)
$$;


--
-- Name: slath_logo_blob_ref(); Type: FUNCTION; Schema: public; Owner: -
--

CREATE FUNCTION public.slath_logo_blob_ref() RETURNS trigger
    LANGUAGE plpgsql
    AS $$
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


--
-- Name: slath_origin_least(text, text); Type: FUNCTION; Schema: public; Owner: -
--

CREATE FUNCTION public.slath_origin_least(a text, b text) RETURNS text
    LANGUAGE sql IMMUTABLE
    AS $$
    SELECT CASE WHEN slath_origin_rank(a) <= slath_origin_rank(b) THEN a ELSE b END
$$;


--
-- Name: slath_origin_rank(text); Type: FUNCTION; Schema: public; Owner: -
--

CREATE FUNCTION public.slath_origin_rank(o text) RETURNS integer
    LANGUAGE sql IMMUTABLE
    AS $$
    SELECT CASE o
        WHEN 'anonymous' THEN 0
        WHEN 'outside_contributor' THEN 1
        WHEN 'bot' THEN 2
        WHEN 'member' THEN 3
        WHEN 'writer' THEN 4
    END
$$;


--
-- Name: slath_owns_namespace(uuid, uuid); Type: FUNCTION; Schema: public; Owner: -
--

CREATE FUNCTION public.slath_owns_namespace(ns uuid, viewer uuid) RETURNS boolean
    LANGUAGE sql STABLE
    AS $$
    SELECT EXISTS (
        SELECT 1 FROM users u WHERE u.namespace_id = ns AND (
            u.id = viewer
            OR (u.kind = 'bot' AND u.bot_owner_kind = 'user' AND u.bot_owner_id = viewer)
            OR (u.kind = 'bot' AND u.bot_owner_kind = 'org' AND EXISTS (
                SELECT 1 FROM org_members m
                WHERE m.org_id = u.bot_owner_id AND m.user_id = viewer AND m.role = 'owner'))
        )
    )
$$;


SET default_tablespace = '';

SET default_table_access_method = heap;

--
-- Name: _sqlx_migrations; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public._sqlx_migrations (
    version bigint NOT NULL,
    description text NOT NULL,
    installed_on timestamp with time zone DEFAULT now() NOT NULL,
    success boolean NOT NULL,
    checksum bytea NOT NULL,
    execution_time bigint NOT NULL
);


--
-- Name: access_tokens; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.access_tokens (
    id uuid NOT NULL,
    user_id uuid NOT NULL,
    name text NOT NULL,
    token_prefix text NOT NULL,
    token_hash bytea NOT NULL,
    scopes text[] NOT NULL,
    repo_ids uuid[],
    expires_at timestamp with time zone,
    last_used_at timestamp with time zone,
    revoked_at timestamp with time zone,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT access_tokens_name_check CHECK (((length(name) >= 1) AND (length(name) <= 100))),
    CONSTRAINT access_tokens_scopes_check CHECK ((cardinality(scopes) > 0)),
    CONSTRAINT access_tokens_token_hash_check CHECK ((length(token_hash) = 32)),
    CONSTRAINT access_tokens_token_prefix_check CHECK ((length(token_prefix) = 8))
);


--
-- Name: attachments; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.attachments (
    id uuid NOT NULL,
    repo_id uuid NOT NULL,
    uploader_id uuid,
    blob_sha256 text NOT NULL,
    filename text NOT NULL,
    content_type text NOT NULL,
    size_bytes bigint NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT attachments_filename_check CHECK (((length(filename) >= 1) AND (length(filename) <= 255))),
    CONSTRAINT attachments_size_bytes_check CHECK ((size_bytes >= 0))
);


--
-- Name: audit_events; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.audit_events (
    id uuid NOT NULL,
    actor_id uuid,
    actor_ip inet,
    action text NOT NULL,
    target_kind text NOT NULL,
    target_id uuid,
    repo_id uuid,
    org_id uuid,
    data jsonb DEFAULT '{}'::jsonb NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL
);


--
-- Name: background_migrations; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.background_migrations (
    name text NOT NULL,
    introduced_in text NOT NULL,
    state text DEFAULT 'pending'::text NOT NULL,
    cursor jsonb,
    processed bigint DEFAULT 0 NOT NULL,
    total_estimate bigint,
    last_error text,
    started_at timestamp with time zone,
    finished_at timestamp with time zone,
    updated_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT background_migrations_state_check CHECK ((state = ANY (ARRAY['pending'::text, 'running'::text, 'paused'::text, 'done'::text, 'failed'::text])))
);


--
-- Name: blobs; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.blobs (
    sha256 text NOT NULL,
    size_bytes bigint NOT NULL,
    refcount integer DEFAULT 0 NOT NULL,
    unreferenced_since timestamp with time zone DEFAULT now(),
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT blobs_refcount_check CHECK ((refcount >= 0)),
    CONSTRAINT blobs_sha256_check CHECK ((sha256 ~ '^[0-9a-f]{64}$'::text)),
    CONSTRAINT blobs_size_bytes_check CHECK ((size_bytes >= 0))
);


--
-- Name: bot_commits; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.bot_commits (
    network_id uuid NOT NULL,
    oid text NOT NULL,
    bot_id uuid NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT bot_commits_oid_check CHECK ((oid ~ '^[0-9a-f]{40}$'::text))
);


--
-- Name: bot_freezes; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.bot_freezes (
    org_id uuid,
    frozen_by uuid,
    frozen_at timestamp with time zone DEFAULT now() NOT NULL
);


--
-- Name: branch_protections; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.branch_protections (
    id uuid NOT NULL,
    repo_id uuid NOT NULL,
    pattern text NOT NULL,
    block_force_push boolean DEFAULT true NOT NULL,
    block_deletion boolean DEFAULT true NOT NULL,
    require_mr boolean DEFAULT false NOT NULL,
    required_approvals integer DEFAULT 1 NOT NULL,
    required_status_contexts text[] DEFAULT '{}'::text[] NOT NULL,
    push_role text DEFAULT 'write'::text NOT NULL,
    dismiss_stale_approvals boolean DEFAULT true NOT NULL,
    statuses_on_head_ok boolean DEFAULT true NOT NULL,
    require_resolved_threads boolean DEFAULT false NOT NULL,
    bots_count_as_approvers boolean DEFAULT false NOT NULL,
    bot_merge_allowlist uuid[] DEFAULT '{}'::uuid[] NOT NULL,
    require_up_to_date boolean DEFAULT false NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    status_reporters jsonb DEFAULT '{}'::jsonb NOT NULL,
    dismiss_approvals_on_new_revision boolean DEFAULT false NOT NULL,
    ready_waits_for_checks boolean DEFAULT true NOT NULL,
    CONSTRAINT branch_protections_pattern_check CHECK (((length(pattern) >= 1) AND (length(pattern) <= 255))),
    CONSTRAINT branch_protections_push_role_check CHECK ((push_role = ANY (ARRAY['write'::text, 'maintain'::text, 'admin'::text, 'nobody'::text]))),
    CONSTRAINT branch_protections_required_approvals_check CHECK (((required_approvals >= 0) AND (required_approvals <= 20)))
);


--
-- Name: comments; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.comments (
    id uuid NOT NULL,
    repo_id uuid NOT NULL,
    subject_kind text NOT NULL,
    subject_id uuid NOT NULL,
    author_id uuid,
    body text NOT NULL,
    edited_at timestamp with time zone,
    deleted_at timestamp with time zone,
    hidden_at timestamp with time zone,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    search tsvector GENERATED ALWAYS AS (to_tsvector('simple'::regconfig, body)) STORED,
    origin text NOT NULL,
    CONSTRAINT comments_origin_check CHECK ((public.slath_origin_rank(origin) IS NOT NULL)),
    CONSTRAINT comments_subject_kind_check CHECK ((subject_kind = ANY (ARRAY['issue'::text, 'merge_request'::text])))
);


--
-- Name: commit_statuses; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.commit_statuses (
    id uuid NOT NULL,
    repo_id uuid NOT NULL,
    commit_oid text NOT NULL,
    context text NOT NULL,
    state text NOT NULL,
    description text,
    target_url text,
    creator_kind text NOT NULL,
    creator_id uuid,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    origin text NOT NULL,
    CONSTRAINT commit_statuses_commit_oid_check CHECK ((commit_oid ~ '^[0-9a-f]{40}$'::text)),
    CONSTRAINT commit_statuses_context_check CHECK ((context ~ '^[A-Za-z0-9 ._/:-]{1,100}$'::text)),
    CONSTRAINT commit_statuses_creator_kind_check CHECK ((creator_kind = ANY (ARRAY['user'::text, 'token'::text, 'ci'::text]))),
    CONSTRAINT commit_statuses_description_check CHECK ((length(description) <= 1000)),
    CONSTRAINT commit_statuses_origin_check CHECK ((public.slath_origin_rank(origin) IS NOT NULL)),
    CONSTRAINT commit_statuses_state_check CHECK ((state = ANY (ARRAY['pending'::text, 'success'::text, 'failure'::text, 'error'::text]))),
    CONSTRAINT commit_statuses_target_url_check CHECK ((length(target_url) <= 2048))
);


--
-- Name: email_verifications; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.email_verifications (
    token_hash bytea NOT NULL,
    user_id uuid NOT NULL,
    email public.citext NOT NULL,
    expires_at timestamp with time zone NOT NULL,
    used_at timestamp with time zone,
    created_at timestamp with time zone DEFAULT now() NOT NULL
);


--
-- Name: encryption_keys; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.encryption_keys (
    key_id text NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    retired_at timestamp with time zone,
    CONSTRAINT encryption_keys_key_id_check CHECK ((key_id ~ '^[0-9a-f]{8}$'::text))
);


--
-- Name: git_refs; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.git_refs (
    repo_id uuid NOT NULL,
    name bytea NOT NULL,
    oid text NOT NULL,
    updated_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT git_refs_oid_check CHECK ((oid ~ '^[0-9a-f]{40}$'::text))
);


--
-- Name: idempotency_keys; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.idempotency_keys (
    principal_id uuid NOT NULL,
    key text NOT NULL,
    method text NOT NULL,
    path text NOT NULL,
    request_sha256 bytea NOT NULL,
    status integer,
    response jsonb,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT idempotency_keys_key_check CHECK ((key ~ '^[A-Za-z0-9_-]{1,128}$'::text))
);


--
-- Name: instance_logos; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.instance_logos (
    theme text NOT NULL,
    blob_sha256 text NOT NULL,
    content_type text NOT NULL,
    updated_by uuid,
    updated_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT instance_logos_content_type_check CHECK ((content_type = ANY (ARRAY['image/svg+xml'::text, 'image/png'::text]))),
    CONSTRAINT instance_logos_theme_check CHECK ((theme = ANY (ARRAY['light'::text, 'dark'::text])))
);


--
-- Name: instance_settings; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.instance_settings (
    key text NOT NULL,
    value jsonb NOT NULL,
    updated_at timestamp with time zone DEFAULT now() NOT NULL
);


--
-- Name: invitations; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.invitations (
    id uuid NOT NULL,
    token_hash bytea NOT NULL,
    created_by uuid,
    email public.citext,
    note text,
    expires_at timestamp with time zone NOT NULL,
    used_at timestamp with time zone,
    used_by uuid,
    revoked_at timestamp with time zone,
    created_at timestamp with time zone DEFAULT now() NOT NULL
);


--
-- Name: issue_assignees; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.issue_assignees (
    issue_id uuid NOT NULL,
    user_id uuid NOT NULL
);


--
-- Name: issue_labels; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.issue_labels (
    issue_id uuid NOT NULL,
    label_id uuid NOT NULL
);


--
-- Name: issues; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.issues (
    id uuid NOT NULL,
    repo_id uuid NOT NULL,
    number integer NOT NULL,
    author_id uuid,
    title text NOT NULL,
    body text DEFAULT ''::text NOT NULL,
    state text DEFAULT 'open'::text NOT NULL,
    state_reason text,
    milestone_id uuid,
    closed_at timestamp with time zone,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    updated_at timestamp with time zone DEFAULT now() NOT NULL,
    search tsvector GENERATED ALWAYS AS ((setweight(to_tsvector('simple'::regconfig, title), 'A'::"char") || setweight(to_tsvector('simple'::regconfig, body), 'B'::"char"))) STORED,
    origin text NOT NULL,
    CONSTRAINT issues_number_check CHECK ((number > 0)),
    CONSTRAINT issues_origin_check CHECK ((public.slath_origin_rank(origin) IS NOT NULL)),
    CONSTRAINT issues_state_check CHECK ((state = ANY (ARRAY['open'::text, 'closed'::text]))),
    CONSTRAINT issues_state_reason_check CHECK ((state_reason = ANY (ARRAY['completed'::text, 'not_planned'::text, 'duplicate'::text]))),
    CONSTRAINT issues_title_check CHECK (((length(title) >= 1) AND (length(title) <= 256)))
);


--
-- Name: jobs; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.jobs (
    id bigint NOT NULL,
    queue text NOT NULL,
    kind text NOT NULL,
    payload jsonb DEFAULT '{}'::jsonb NOT NULL,
    dedupe_key text,
    state text DEFAULT 'ready'::text NOT NULL,
    run_at timestamp with time zone DEFAULT now() NOT NULL,
    attempts integer DEFAULT 0 NOT NULL,
    max_attempts integer DEFAULT 8 NOT NULL,
    locked_by text,
    locked_until timestamp with time zone,
    last_error text,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    finished_at timestamp with time zone,
    CONSTRAINT jobs_max_attempts_check CHECK ((max_attempts > 0)),
    CONSTRAINT jobs_state_check CHECK ((state = ANY (ARRAY['ready'::text, 'running'::text, 'done'::text, 'dead'::text])))
);


--
-- Name: jobs_id_seq; Type: SEQUENCE; Schema: public; Owner: -
--

CREATE SEQUENCE public.jobs_id_seq
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;


--
-- Name: jobs_id_seq; Type: SEQUENCE OWNED BY; Schema: public; Owner: -
--

ALTER SEQUENCE public.jobs_id_seq OWNED BY public.jobs.id;


--
-- Name: labels; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.labels (
    id uuid NOT NULL,
    repo_id uuid NOT NULL,
    name public.citext NOT NULL,
    color character(6) NOT NULL,
    description text,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    origin text NOT NULL,
    CONSTRAINT labels_color_check CHECK ((color ~ '^[0-9a-f]{6}$'::text)),
    CONSTRAINT labels_name_check CHECK (((length((name)::text) >= 1) AND (length((name)::text) <= 50))),
    CONSTRAINT labels_origin_check CHECK ((public.slath_origin_rank(origin) IS NOT NULL))
);


--
-- Name: login_attempts; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.login_attempts (
    key text NOT NULL,
    window_start timestamp with time zone NOT NULL,
    count integer DEFAULT 0 NOT NULL,
    CONSTRAINT login_attempts_key_check CHECK ((length(key) <= 200))
);


--
-- Name: login_challenges; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.login_challenges (
    id_hash bytea NOT NULL,
    user_id uuid NOT NULL,
    purpose text NOT NULL,
    first_factor text NOT NULL,
    data jsonb DEFAULT '{}'::jsonb NOT NULL,
    failures integer DEFAULT 0 NOT NULL,
    expires_at timestamp with time zone NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT login_challenges_first_factor_check CHECK ((first_factor = ANY (ARRAY['password'::text, 'oidc'::text, 'reset'::text]))),
    CONSTRAINT login_challenges_purpose_check CHECK ((purpose = ANY (ARRAY['login'::text, 'sudo'::text])))
);


--
-- Name: merge_requests; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.merge_requests (
    id uuid NOT NULL,
    repo_id uuid NOT NULL,
    number integer NOT NULL,
    author_id uuid,
    title text NOT NULL,
    description text DEFAULT ''::text NOT NULL,
    draft boolean DEFAULT false NOT NULL,
    target_branch text NOT NULL,
    source_repo_id uuid,
    source_ref bytea NOT NULL,
    allow_maintainer_edits boolean DEFAULT true NOT NULL,
    state text DEFAULT 'open'::text NOT NULL,
    milestone_id uuid,
    auto_merge_strategy text,
    auto_merge_by uuid,
    auto_merge_set_at timestamp with time zone,
    delete_source_on_merge boolean DEFAULT true NOT NULL,
    merged_by uuid,
    merged_at timestamp with time zone,
    merge_strategy text,
    merged_oid text,
    closed_at timestamp with time zone,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    updated_at timestamp with time zone DEFAULT now() NOT NULL,
    search tsvector GENERATED ALWAYS AS ((setweight(to_tsvector('simple'::regconfig, title), 'A'::"char") || setweight(to_tsvector('simple'::regconfig, description), 'B'::"char"))) STORED,
    source_findings jsonb,
    attention text DEFAULT 'ready'::text NOT NULL,
    origin text NOT NULL,
    CONSTRAINT merge_requests_attention_check CHECK ((attention = ANY (ARRAY['iterating'::text, 'waiting_for_checks'::text, 'ready'::text]))),
    CONSTRAINT merge_requests_auto_merge_strategy_check CHECK ((auto_merge_strategy = ANY (ARRAY['fast-forward'::text, 'rebase'::text, 'merge-commit'::text, 'squash'::text]))),
    CONSTRAINT merge_requests_check CHECK (((state = 'merged'::text) = (merged_at IS NOT NULL))),
    CONSTRAINT merge_requests_merge_strategy_check CHECK ((merge_strategy = ANY (ARRAY['fast-forward'::text, 'rebase'::text, 'merge-commit'::text, 'squash'::text]))),
    CONSTRAINT merge_requests_merged_oid_check CHECK ((merged_oid ~ '^[0-9a-f]{40}$'::text)),
    CONSTRAINT merge_requests_number_check CHECK ((number > 0)),
    CONSTRAINT merge_requests_origin_check CHECK ((public.slath_origin_rank(origin) IS NOT NULL)),
    CONSTRAINT merge_requests_state_check CHECK ((state = ANY (ARRAY['open'::text, 'merged'::text, 'closed'::text]))),
    CONSTRAINT merge_requests_target_branch_check CHECK (((length(target_branch) >= 1) AND (length(target_branch) <= 255))),
    CONSTRAINT merge_requests_title_check CHECK (((length(title) >= 1) AND (length(title) <= 256)))
);


--
-- Name: milestones; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.milestones (
    id uuid NOT NULL,
    repo_id uuid NOT NULL,
    title public.citext NOT NULL,
    description text,
    due_on date,
    state text DEFAULT 'open'::text NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    updated_at timestamp with time zone DEFAULT now() NOT NULL,
    origin text NOT NULL,
    CONSTRAINT milestones_origin_check CHECK ((public.slath_origin_rank(origin) IS NOT NULL)),
    CONSTRAINT milestones_state_check CHECK ((state = ANY (ARRAY['open'::text, 'closed'::text]))),
    CONSTRAINT milestones_title_check CHECK (((length((title)::text) >= 1) AND (length((title)::text) <= 100)))
);


--
-- Name: mr_assignees; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.mr_assignees (
    mr_id uuid NOT NULL,
    user_id uuid NOT NULL
);


--
-- Name: mr_change_aliases; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.mr_change_aliases (
    mr_id uuid NOT NULL,
    kind text NOT NULL,
    value text NOT NULL,
    change_id uuid NOT NULL,
    revision_seq integer NOT NULL,
    bound_by uuid,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT mr_change_aliases_kind_check CHECK ((kind = ANY (ARRAY['jj'::text, 'gerrit'::text]))),
    CONSTRAINT mr_change_aliases_value_check CHECK ((value ~ '^([k-z]{32}|I[0-9a-f]{40})$'::text))
);


--
-- Name: mr_change_versions; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.mr_change_versions (
    id uuid NOT NULL,
    revision_id uuid NOT NULL,
    change_id uuid,
    "position" integer NOT NULL,
    commit_oid text NOT NULL,
    parent_oid text,
    patch_id text,
    state text NOT NULL,
    reason text,
    candidates uuid[] DEFAULT '{}'::uuid[] NOT NULL,
    conflicted boolean DEFAULT false NOT NULL,
    CONSTRAINT mr_change_versions_check CHECK (((state = 'unresolved'::text) = (change_id IS NULL))),
    CONSTRAINT mr_change_versions_commit_oid_check CHECK ((commit_oid ~ '^[0-9a-f]{40}$'::text)),
    CONSTRAINT mr_change_versions_parent_oid_check CHECK ((parent_oid ~ '^[0-9a-f]{40}$'::text)),
    CONSTRAINT mr_change_versions_patch_id_check CHECK ((patch_id ~ '^[0-9a-f]{40}$'::text)),
    CONSTRAINT mr_change_versions_position_check CHECK (("position" >= 0)),
    CONSTRAINT mr_change_versions_state_check CHECK ((state = ANY (ARRAY['new'::text, 'confirmed'::text, 'inferred'::text, 'unresolved'::text])))
);


--
-- Name: mr_changes; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.mr_changes (
    id uuid NOT NULL,
    mr_id uuid NOT NULL,
    display_id text NOT NULL,
    state text DEFAULT 'active'::text NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT mr_changes_display_id_check CHECK ((display_id ~ '^[k-z]{32}$'::text)),
    CONSTRAINT mr_changes_state_check CHECK ((state = ANY (ARRAY['active'::text, 'removed'::text, 'merged'::text])))
);


--
-- Name: mr_checkouts; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.mr_checkouts (
    mr_id uuid NOT NULL,
    user_id uuid NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL
);


--
-- Name: mr_labels; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.mr_labels (
    mr_id uuid NOT NULL,
    label_id uuid NOT NULL
);


--
-- Name: mr_ledger; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.mr_ledger (
    id uuid NOT NULL,
    mr_id uuid NOT NULL,
    revision_seq integer,
    kind text NOT NULL,
    change_id text,
    state text,
    reason text,
    detail jsonb DEFAULT '{}'::jsonb NOT NULL,
    actor_id uuid,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT mr_ledger_change_id_check CHECK ((change_id ~ '^[k-z]{32}$'::text)),
    CONSTRAINT mr_ledger_kind_check CHECK ((kind = ANY (ARRAY['binding'::text, 'approval_carried'::text, 'approval_stale'::text, 'status_stale'::text, 'target_moved'::text, 'signature_removed'::text])))
);


--
-- Name: mr_review_viewed_files; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.mr_review_viewed_files (
    review_id uuid NOT NULL,
    path text NOT NULL
);


--
-- Name: mr_reviewers; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.mr_reviewers (
    id uuid NOT NULL,
    mr_id uuid NOT NULL,
    user_id uuid,
    team_id uuid,
    requested_by uuid,
    state text DEFAULT 'requested'::text NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT mr_reviewers_check CHECK (((user_id IS NULL) <> (team_id IS NULL))),
    CONSTRAINT mr_reviewers_state_check CHECK ((state = ANY (ARRAY['requested'::text, 'reviewed'::text, 'removed'::text])))
);


--
-- Name: mr_reviews; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.mr_reviews (
    id uuid NOT NULL,
    mr_id uuid NOT NULL,
    revision_seq integer NOT NULL,
    reviewer_id uuid,
    verdict text NOT NULL,
    body text DEFAULT ''::text NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    files_total integer,
    origin text NOT NULL,
    CONSTRAINT mr_reviews_files_total_check CHECK ((files_total >= 0)),
    CONSTRAINT mr_reviews_origin_check CHECK ((public.slath_origin_rank(origin) IS NOT NULL)),
    CONSTRAINT mr_reviews_revision_seq_check CHECK ((revision_seq > 0)),
    CONSTRAINT mr_reviews_verdict_check CHECK ((verdict = ANY (ARRAY['approve'::text, 'request_changes'::text, 'comment'::text])))
);


--
-- Name: mr_revisions; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.mr_revisions (
    id uuid NOT NULL,
    mr_id uuid NOT NULL,
    seq integer NOT NULL,
    base_oid text NOT NULL,
    merge_base_oid text,
    head_oid text NOT NULL,
    pushed_by uuid,
    commit_count integer DEFAULT 0 NOT NULL,
    mode text DEFAULT 'per-change'::text NOT NULL,
    mode_reason text,
    missing_objects boolean DEFAULT false NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    same_patches boolean,
    CONSTRAINT mr_revisions_base_oid_check CHECK ((base_oid ~ '^[0-9a-f]{40}$'::text)),
    CONSTRAINT mr_revisions_commit_count_check CHECK ((commit_count >= 0)),
    CONSTRAINT mr_revisions_head_oid_check CHECK ((head_oid ~ '^[0-9a-f]{40}$'::text)),
    CONSTRAINT mr_revisions_merge_base_oid_check CHECK ((merge_base_oid ~ '^[0-9a-f]{40}$'::text)),
    CONSTRAINT mr_revisions_mode_check CHECK ((mode = ANY (ARRAY['per-change'::text, 'whole-only'::text]))),
    CONSTRAINT mr_revisions_seq_check CHECK ((seq > 0))
);


--
-- Name: mr_thread_comments; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.mr_thread_comments (
    id uuid NOT NULL,
    thread_id uuid NOT NULL,
    author_id uuid,
    body text NOT NULL,
    pending boolean DEFAULT false NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    edited_at timestamp with time zone,
    deleted_at timestamp with time zone,
    origin text NOT NULL,
    resolves boolean DEFAULT false NOT NULL,
    CONSTRAINT mr_thread_comments_origin_check CHECK ((public.slath_origin_rank(origin) IS NOT NULL)),
    CONSTRAINT mr_thread_comments_resolves_pending CHECK ((pending OR (NOT resolves)))
);


--
-- Name: mr_thread_findings; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.mr_thread_findings (
    thread_id uuid NOT NULL,
    mr_id uuid NOT NULL,
    revision_seq integer NOT NULL,
    context text NOT NULL,
    key text NOT NULL,
    severity text NOT NULL,
    category text NOT NULL,
    status_id uuid,
    resolved_by_status uuid,
    CONSTRAINT mr_thread_findings_context_check CHECK ((context ~ '^[A-Za-z0-9 ._/:-]{1,100}$'::text)),
    CONSTRAINT mr_thread_findings_key_check CHECK ((key ~ '^[0-9a-f]{64}$'::text)),
    CONSTRAINT mr_thread_findings_revision_seq_check CHECK ((revision_seq > 0)),
    CONSTRAINT mr_thread_findings_severity_check CHECK ((severity = ANY (ARRAY['notice'::text, 'warning'::text, 'failure'::text])))
);


--
-- Name: mr_threads; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.mr_threads (
    id uuid NOT NULL,
    mr_id uuid NOT NULL,
    revision_seq integer NOT NULL,
    old_oid text,
    new_oid text,
    path text,
    side text,
    line_start integer,
    line_end integer,
    line_hash text,
    context jsonb DEFAULT '{}'::jsonb NOT NULL,
    author_id uuid,
    resolved_at timestamp with time zone,
    resolved_by uuid,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    addressed_by uuid,
    addressed_at timestamp with time zone,
    CONSTRAINT mr_threads_check CHECK ((((path IS NULL) = (side IS NULL)) AND ((path IS NULL) = (line_start IS NULL)))),
    CONSTRAINT mr_threads_check1 CHECK (((line_end IS NULL) OR (line_end >= line_start))),
    CONSTRAINT mr_threads_line_hash_check CHECK ((line_hash ~ '^[0-9a-f]{64}$'::text)),
    CONSTRAINT mr_threads_line_start_check CHECK ((line_start > 0)),
    CONSTRAINT mr_threads_new_oid_check CHECK ((new_oid ~ '^[0-9a-f]{40}$'::text)),
    CONSTRAINT mr_threads_old_oid_check CHECK ((old_oid ~ '^[0-9a-f]{40}$'::text)),
    CONSTRAINT mr_threads_path_check CHECK (((length(path) >= 1) AND (length(path) <= 4096))),
    CONSTRAINT mr_threads_revision_seq_check CHECK ((revision_seq > 0)),
    CONSTRAINT mr_threads_side_check CHECK ((side = ANY (ARRAY['old'::text, 'new'::text])))
);


--
-- Name: mr_viewed_files; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.mr_viewed_files (
    mr_id uuid NOT NULL,
    user_id uuid NOT NULL,
    path text NOT NULL,
    blob_oid text NOT NULL,
    CONSTRAINT mr_viewed_files_blob_oid_check CHECK ((blob_oid ~ '^[0-9a-f]{40}$'::text))
);


--
-- Name: namespaces; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.namespaces (
    id uuid NOT NULL,
    name public.citext NOT NULL,
    kind text NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT namespaces_kind_check CHECK ((kind = ANY (ARRAY['user'::text, 'org'::text])))
);


--
-- Name: notifications; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.notifications (
    id uuid NOT NULL,
    user_id uuid NOT NULL,
    repo_id uuid NOT NULL,
    subject_kind text NOT NULL,
    subject_id uuid NOT NULL,
    reason text NOT NULL,
    unread boolean DEFAULT true NOT NULL,
    updated_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT notifications_reason_check CHECK ((reason = ANY (ARRAY['author'::text, 'assigned'::text, 'mentioned'::text, 'review_requested'::text, 'mr_ready'::text, 'auto_merge_result'::text, 'commented'::text, 'watching'::text, 'state_change'::text, 'ci_failed'::text]))),
    CONSTRAINT notifications_subject_kind_check CHECK ((subject_kind = ANY (ARRAY['issue'::text, 'merge_request'::text])))
);


--
-- Name: oidc_identities; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.oidc_identities (
    issuer text NOT NULL,
    subject text NOT NULL,
    user_id uuid NOT NULL,
    provider_key text NOT NULL,
    email text,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    last_login_at timestamp with time zone
);


--
-- Name: oidc_pending; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.oidc_pending (
    token_hash text NOT NULL,
    provider_key text NOT NULL,
    browser_hash text NOT NULL,
    issuer text NOT NULL,
    subject text NOT NULL,
    email text,
    email_verified boolean NOT NULL,
    preferred_username text,
    display_name text,
    expires_at timestamp with time zone NOT NULL
);


--
-- Name: oidc_states; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.oidc_states (
    state_hash text NOT NULL,
    provider_key text NOT NULL,
    browser_hash text NOT NULL,
    nonce text NOT NULL,
    pkce_verifier text NOT NULL,
    purpose text NOT NULL,
    user_id uuid,
    next text,
    expires_at timestamp with time zone NOT NULL,
    CONSTRAINT oidc_states_purpose_check CHECK ((purpose = ANY (ARRAY['login'::text, 'link'::text])))
);


--
-- Name: org_members; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.org_members (
    org_id uuid NOT NULL,
    user_id uuid NOT NULL,
    role text NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT org_members_role_check CHECK ((role = ANY (ARRAY['owner'::text, 'member'::text])))
);


--
-- Name: organizations; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.organizations (
    id uuid NOT NULL,
    namespace_id uuid NOT NULL,
    display_name text,
    description text,
    default_member_role text DEFAULT 'none'::text NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    updated_at timestamp with time zone DEFAULT now() NOT NULL,
    require_mfa boolean DEFAULT false NOT NULL,
    avatar_blob_sha256 text,
    CONSTRAINT organizations_default_member_role_check CHECK ((default_member_role = ANY (ARRAY['none'::text, 'read'::text]))),
    CONSTRAINT organizations_description_check CHECK ((length(description) <= 500)),
    CONSTRAINT organizations_display_name_check CHECK ((length(display_name) <= 100))
);


--
-- Name: password_resets; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.password_resets (
    token_hash bytea NOT NULL,
    user_id uuid NOT NULL,
    expires_at timestamp with time zone NOT NULL,
    used_at timestamp with time zone,
    created_at timestamp with time zone DEFAULT now() NOT NULL
);


--
-- Name: push_events; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.push_events (
    id uuid NOT NULL,
    repo_id uuid NOT NULL,
    pusher_id uuid,
    transport text NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT push_events_transport_check CHECK ((transport = ANY (ARRAY['http'::text, 'ssh'::text, 'internal'::text, 'reconcile'::text])))
);


--
-- Name: recovery_codes; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.recovery_codes (
    id uuid NOT NULL,
    user_id uuid NOT NULL,
    code_hash text NOT NULL,
    used_at timestamp with time zone,
    created_at timestamp with time zone DEFAULT now() NOT NULL
);


--
-- Name: ref_updates; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.ref_updates (
    push_id uuid NOT NULL,
    seq integer NOT NULL,
    name bytea NOT NULL,
    old_oid text,
    new_oid text,
    CONSTRAINT ref_updates_new_oid_check CHECK ((new_oid ~ '^[0-9a-f]{40}$'::text)),
    CONSTRAINT ref_updates_old_oid_check CHECK ((old_oid ~ '^[0-9a-f]{40}$'::text))
);


--
-- Name: repo_collaborators; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.repo_collaborators (
    repo_id uuid NOT NULL,
    user_id uuid NOT NULL,
    role text NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT repo_collaborators_role_check CHECK ((role = ANY (ARRAY['read'::text, 'write'::text, 'maintain'::text, 'admin'::text])))
);


--
-- Name: repo_forks; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.repo_forks (
    repo_id uuid NOT NULL,
    parent_id uuid,
    default_branch_only boolean DEFAULT false NOT NULL,
    max_visibility text NOT NULL,
    state text DEFAULT 'queued'::text NOT NULL,
    error text,
    requested_by uuid,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    finished_at timestamp with time zone,
    CONSTRAINT repo_forks_error_check CHECK ((length(error) <= 500)),
    CONSTRAINT repo_forks_max_visibility_check CHECK ((max_visibility = ANY (ARRAY['private'::text, 'internal'::text, 'public'::text]))),
    CONSTRAINT repo_forks_state_check CHECK ((state = ANY (ARRAY['queued'::text, 'running'::text, 'done'::text, 'failed'::text])))
);


--
-- Name: repo_imports; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.repo_imports (
    repo_id uuid NOT NULL,
    url text NOT NULL,
    requested_by uuid,
    state text DEFAULT 'queued'::text NOT NULL,
    error text,
    branches integer,
    tags integer,
    size_bytes bigint,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    started_at timestamp with time zone,
    finished_at timestamp with time zone,
    secret_findings integer,
    secret_findings_allowed integer,
    CONSTRAINT repo_imports_secret_findings_allowed_check CHECK ((secret_findings_allowed >= 0)),
    CONSTRAINT repo_imports_secret_findings_check CHECK ((secret_findings >= 0)),
    CONSTRAINT repo_imports_state_check CHECK ((state = ANY (ARRAY['queued'::text, 'running'::text, 'done'::text, 'failed'::text])))
);


--
-- Name: repo_redirects; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.repo_redirects (
    namespace_id uuid NOT NULL,
    name public.citext NOT NULL,
    repo_id uuid NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL
);


--
-- Name: repo_team_access; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.repo_team_access (
    repo_id uuid NOT NULL,
    team_id uuid NOT NULL,
    role text NOT NULL,
    CONSTRAINT repo_team_access_role_check CHECK ((role = ANY (ARRAY['read'::text, 'write'::text, 'maintain'::text, 'admin'::text])))
);


--
-- Name: repositories; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.repositories (
    id uuid NOT NULL,
    namespace_id uuid NOT NULL,
    name public.citext NOT NULL,
    description text,
    visibility text NOT NULL,
    default_branch text DEFAULT 'main'::text NOT NULL,
    state text DEFAULT 'active'::text NOT NULL,
    fork_parent_id uuid,
    fork_network_id uuid NOT NULL,
    allow_forking boolean DEFAULT true NOT NULL,
    trust_bot_runs boolean DEFAULT false NOT NULL,
    auto_approve_external_for text DEFAULT 'none'::text NOT NULL,
    issues_enabled boolean DEFAULT true NOT NULL,
    merge_requests_enabled boolean DEFAULT true NOT NULL,
    size_bytes bigint DEFAULT 0 NOT NULL,
    next_item_number integer DEFAULT 1 NOT NULL,
    merge_strategies text[] DEFAULT '{fast-forward,rebase,merge-commit,squash}'::text[] NOT NULL,
    default_merge_strategy text DEFAULT 'rebase'::text NOT NULL,
    delete_source_branch_default boolean DEFAULT true NOT NULL,
    archived_reason text,
    deleted_at timestamp with time zone,
    pushed_at timestamp with time zone,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    updated_at timestamp with time zone DEFAULT now() NOT NULL,
    pushes_since_maintenance integer DEFAULT 0 NOT NULL,
    maintained_at timestamp with time zone,
    maintenance_runs integer DEFAULT 0 NOT NULL,
    maintenance_lease_until timestamp with time zone,
    push_protection text DEFAULT 'block'::text NOT NULL,
    secret_allowance_role text DEFAULT 'write'::text NOT NULL,
    sensitive_paths text[] DEFAULT '{}'::text[] NOT NULL,
    description_origin text NOT NULL,
    CONSTRAINT repositories_archived_reason_check CHECK ((archived_reason = ANY (ARRAY['user'::text, 'lost_parent_access'::text]))),
    CONSTRAINT repositories_default_branch_check CHECK (((length(default_branch) >= 1) AND (length(default_branch) <= 255))),
    CONSTRAINT repositories_description_check CHECK ((length(description) <= 500)),
    CONSTRAINT repositories_description_origin_check CHECK ((public.slath_origin_rank(description_origin) IS NOT NULL)),
    CONSTRAINT repositories_push_protection_check CHECK ((push_protection = ANY (ARRAY['block'::text, 'warn'::text, 'off'::text]))),
    CONSTRAINT repositories_secret_allowance_role_check CHECK ((secret_allowance_role = ANY (ARRAY['write'::text, 'admin'::text]))),
    CONSTRAINT repositories_sensitive_paths_check CHECK ((cardinality(sensitive_paths) <= 100)),
    CONSTRAINT repositories_state_check CHECK ((state = ANY (ARRAY['active'::text, 'archived'::text, 'deleting'::text, 'locked'::text]))),
    CONSTRAINT repositories_visibility_check CHECK ((visibility = ANY (ARRAY['public'::text, 'internal'::text, 'private'::text])))
);


--
-- Name: reserved_names; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.reserved_names (
    name public.citext NOT NULL
);


--
-- Name: schema_info; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.schema_info (
    version integer NOT NULL,
    singleton boolean DEFAULT true NOT NULL,
    CONSTRAINT schema_info_singleton_check CHECK (singleton)
);


--
-- Name: secret_allowances; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.secret_allowances (
    id uuid NOT NULL,
    repo_id uuid NOT NULL,
    fingerprint text NOT NULL,
    rule text NOT NULL,
    created_by uuid,
    reason text,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    expires_at timestamp with time zone NOT NULL,
    description text,
    CONSTRAINT secret_allowances_check CHECK ((expires_at > created_at)),
    CONSTRAINT secret_allowances_description_check CHECK ((char_length(description) <= 200)),
    CONSTRAINT secret_allowances_fingerprint_check CHECK ((fingerprint ~ '^[0-9a-f]{32}$'::text)),
    CONSTRAINT secret_allowances_reason_check CHECK ((length(reason) <= 500)),
    CONSTRAINT secret_allowances_rule_check CHECK (((length(rule) >= 1) AND (length(rule) <= 100)))
);


--
-- Name: secret_rejections; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.secret_rejections (
    id uuid NOT NULL,
    repo_id uuid NOT NULL,
    findings jsonb NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    expires_at timestamp with time zone NOT NULL,
    kind text NOT NULL,
    CONSTRAINT secret_rejections_kind_check CHECK ((kind = ANY (ARRAY['first_push'::text, 'push'::text])))
);


--
-- Name: sessions; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.sessions (
    id_hash bytea NOT NULL,
    user_id uuid NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    last_seen_at timestamp with time zone DEFAULT now() NOT NULL,
    expires_at timestamp with time zone NOT NULL,
    auth_level text NOT NULL,
    sudo_until timestamp with time zone,
    ip inet,
    user_agent text,
    CONSTRAINT sessions_auth_level_check CHECK ((auth_level = ANY (ARRAY['password'::text, 'mfa'::text, 'passkey'::text, 'oidc'::text]))),
    CONSTRAINT sessions_id_hash_check CHECK ((length(id_hash) = 32)),
    CONSTRAINT sessions_user_agent_check CHECK ((length(user_agent) <= 512))
);


--
-- Name: ssh_keys; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.ssh_keys (
    id uuid NOT NULL,
    user_id uuid,
    title text NOT NULL,
    key_type text NOT NULL,
    public_key text NOT NULL,
    fingerprint_sha256 text NOT NULL,
    last_used_at timestamp with time zone,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    repo_id uuid,
    can_push boolean DEFAULT false NOT NULL,
    added_by uuid,
    CONSTRAINT ssh_keys_fingerprint_sha256_check CHECK ((fingerprint_sha256 ~ '^SHA256:[A-Za-z0-9+/]{43}$'::text)),
    CONSTRAINT ssh_keys_key_type_check CHECK ((key_type ~ '^[A-Za-z0-9.@-]+$'::text)),
    CONSTRAINT ssh_keys_one_owner CHECK (((user_id IS NULL) <> (repo_id IS NULL))),
    CONSTRAINT ssh_keys_public_key_check CHECK ((public_key ~ '^[A-Za-z0-9+/=]+$'::text)),
    CONSTRAINT ssh_keys_push_for_deploy_keys CHECK (((repo_id IS NOT NULL) OR (NOT can_push))),
    CONSTRAINT ssh_keys_title_check CHECK (((length(title) >= 1) AND (length(title) <= 100)))
);


--
-- Name: status_annotations; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.status_annotations (
    status_id uuid NOT NULL,
    ordinal integer NOT NULL,
    path text NOT NULL,
    line_start integer NOT NULL,
    line_end integer NOT NULL,
    severity text NOT NULL,
    category text NOT NULL,
    message text NOT NULL,
    CONSTRAINT status_annotations_category_check CHECK ((category = ANY (ARRAY['correctness'::text, 'security'::text, 'tests'::text, 'dependencies'::text, 'scope'::text, 'maintainability'::text, 'docs'::text, 'other'::text]))),
    CONSTRAINT status_annotations_check CHECK ((line_end >= line_start)),
    CONSTRAINT status_annotations_line_start_check CHECK ((line_start > 0)),
    CONSTRAINT status_annotations_message_check CHECK ((length(message) >= 1)),
    CONSTRAINT status_annotations_ordinal_check CHECK ((ordinal >= 0)),
    CONSTRAINT status_annotations_path_check CHECK (((length(path) >= 1) AND (length(path) <= 4096))),
    CONSTRAINT status_annotations_severity_check CHECK ((severity = ANY (ARRAY['notice'::text, 'warning'::text, 'failure'::text])))
);


--
-- Name: subscriptions; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.subscriptions (
    user_id uuid NOT NULL,
    subject_kind text NOT NULL,
    subject_id uuid NOT NULL,
    repo_id uuid NOT NULL,
    state text NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT subscriptions_state_check CHECK ((state = ANY (ARRAY['watching'::text, 'ignoring'::text]))),
    CONSTRAINT subscriptions_subject_kind_check CHECK ((subject_kind = ANY (ARRAY['repo'::text, 'issue'::text, 'merge_request'::text])))
);


--
-- Name: team_members; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.team_members (
    team_id uuid NOT NULL,
    user_id uuid NOT NULL
);


--
-- Name: teams; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.teams (
    id uuid NOT NULL,
    org_id uuid NOT NULL,
    name public.citext NOT NULL,
    description text,
    created_at timestamp with time zone DEFAULT now() NOT NULL
);


--
-- Name: timeline_events; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.timeline_events (
    id uuid NOT NULL,
    repo_id uuid NOT NULL,
    subject_kind text NOT NULL,
    subject_id uuid NOT NULL,
    actor_id uuid,
    kind text NOT NULL,
    data jsonb DEFAULT '{}'::jsonb NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT timeline_events_kind_check CHECK ((kind = ANY (ARRAY['closed'::text, 'reopened'::text, 'labeled'::text, 'unlabeled'::text, 'assigned'::text, 'unassigned'::text, 'milestoned'::text, 'demilestoned'::text, 'renamed'::text, 'referenced'::text, 'revision'::text, 'ready'::text, 'draft'::text, 'source_deleted'::text, 'source_restored'::text, 'merged'::text, 'review_requested'::text, 'review_request_removed'::text, 'change_split'::text, 'change_joined'::text, 'change_resolved'::text, 'approvals_carried'::text, 'approvals_stale'::text, 'thread_resolved'::text, 'thread_reopened'::text, 'source_published'::text, 'maintainer_edits_allowed'::text, 'maintainer_edits_disallowed'::text, 'branch_updated'::text, 'auto_merge_set'::text, 'auto_merge_cancelled'::text, 'attention_released'::text, 'thread_addressed'::text]))),
    CONSTRAINT timeline_events_subject_kind_check CHECK ((subject_kind = ANY (ARRAY['issue'::text, 'merge_request'::text])))
);


--
-- Name: totp_credentials; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.totp_credentials (
    user_id uuid NOT NULL,
    secret_enc bytea NOT NULL,
    key_id text NOT NULL,
    last_used_step bigint DEFAULT 0 NOT NULL,
    confirmed boolean DEFAULT false NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL
);


--
-- Name: upgrade_history; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.upgrade_history (
    id uuid NOT NULL,
    from_version text,
    to_version text NOT NULL,
    from_schema integer NOT NULL,
    to_schema integer NOT NULL,
    backup_path text,
    preflight jsonb DEFAULT '{}'::jsonb NOT NULL,
    started_at timestamp with time zone DEFAULT now() NOT NULL,
    finished_at timestamp with time zone,
    outcome text,
    error text,
    CONSTRAINT upgrade_history_outcome_check CHECK ((outcome = ANY (ARRAY['success'::text, 'failed'::text, 'aborted'::text])))
);


--
-- Name: user_emails; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.user_emails (
    user_id uuid NOT NULL,
    email public.citext NOT NULL,
    verified boolean DEFAULT false NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL
);


--
-- Name: users; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.users (
    id uuid NOT NULL,
    namespace_id uuid NOT NULL,
    kind text DEFAULT 'human'::text NOT NULL,
    bot_owner_kind text,
    bot_owner_id uuid,
    display_name text,
    email public.citext,
    email_verified boolean DEFAULT false NOT NULL,
    password_hash text,
    is_admin boolean DEFAULT false NOT NULL,
    state text DEFAULT 'active'::text NOT NULL,
    mfa_required boolean DEFAULT false NOT NULL,
    avatar_blob_sha256 text,
    timezone text,
    theme text DEFAULT 'system'::text NOT NULL,
    diff_view text DEFAULT 'unified'::text NOT NULL,
    editor_mode text DEFAULT 'wysiwyg'::text NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    updated_at timestamp with time zone DEFAULT now() NOT NULL,
    density text DEFAULT 'comfortable'::text NOT NULL,
    code_font_size smallint DEFAULT 14 NOT NULL,
    email_notifications boolean DEFAULT true NOT NULL,
    diff_ignore_whitespace boolean DEFAULT false NOT NULL,
    vcs_tool text,
    pushed_tool text,
    suspended_with_owner boolean DEFAULT false NOT NULL,
    stopped_at timestamp with time zone,
    stopped_by uuid,
    CONSTRAINT users_bot_owner_kind_check CHECK ((bot_owner_kind = ANY (ARRAY['user'::text, 'org'::text]))),
    CONSTRAINT users_check CHECK (((kind = 'bot'::text) = ((bot_owner_kind IS NOT NULL) AND (bot_owner_id IS NOT NULL)))),
    CONSTRAINT users_check1 CHECK (((kind = 'human'::text) OR (password_hash IS NULL))),
    CONSTRAINT users_code_font_size_check CHECK (((code_font_size >= 12) AND (code_font_size <= 20))),
    CONSTRAINT users_density_check CHECK ((density = ANY (ARRAY['comfortable'::text, 'compact'::text]))),
    CONSTRAINT users_diff_view_check CHECK ((diff_view = ANY (ARRAY['unified'::text, 'split'::text]))),
    CONSTRAINT users_editor_mode_check CHECK ((editor_mode = ANY (ARRAY['wysiwyg'::text, 'source'::text]))),
    CONSTRAINT users_kind_check CHECK ((kind = ANY (ARRAY['human'::text, 'bot'::text]))),
    CONSTRAINT users_pushed_tool_check CHECK ((pushed_tool = ANY (ARRAY['git'::text, 'jj'::text]))),
    CONSTRAINT users_state_check CHECK ((state = ANY (ARRAY['active'::text, 'suspended'::text, 'deleted'::text]))),
    CONSTRAINT users_stopped_bots CHECK (((stopped_at IS NULL) OR ((kind = 'bot'::text) AND (state = 'suspended'::text)))),
    CONSTRAINT users_theme_check CHECK ((theme = ANY (ARRAY['system'::text, 'light'::text, 'dark'::text]))),
    CONSTRAINT users_vcs_tool_check CHECK ((vcs_tool = ANY (ARRAY['git'::text, 'jj'::text])))
);


--
-- Name: webauthn_ceremonies; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.webauthn_ceremonies (
    id_hash bytea NOT NULL,
    user_id uuid,
    kind text NOT NULL,
    state jsonb NOT NULL,
    expires_at timestamp with time zone NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT webauthn_ceremonies_kind_check CHECK ((kind = ANY (ARRAY['register'::text, 'authenticate'::text, 'discover'::text])))
);


--
-- Name: webauthn_credentials; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.webauthn_credentials (
    id uuid NOT NULL,
    user_id uuid NOT NULL,
    credential_id bytea NOT NULL,
    passkey jsonb NOT NULL,
    name text NOT NULL,
    last_used_at timestamp with time zone,
    created_at timestamp with time zone DEFAULT now() NOT NULL
);


--
-- Name: webhook_attempts; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.webhook_attempts (
    delivery_id uuid NOT NULL,
    attempt integer NOT NULL,
    started_at timestamp with time zone DEFAULT now() NOT NULL,
    duration_ms integer NOT NULL,
    status integer,
    response text,
    error text,
    CONSTRAINT webhook_attempts_attempt_check CHECK ((attempt > 0)),
    CONSTRAINT webhook_attempts_error_check CHECK ((length(error) <= 500)),
    CONSTRAINT webhook_attempts_response_check CHECK ((length(response) <= 4096))
);


--
-- Name: webhook_deliveries; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.webhook_deliveries (
    id uuid NOT NULL,
    webhook_id uuid NOT NULL,
    event text NOT NULL,
    action text,
    payload jsonb NOT NULL,
    state text DEFAULT 'pending'::text NOT NULL,
    attempts integer DEFAULT 0 NOT NULL,
    next_attempt_at timestamp with time zone,
    redelivery_of uuid,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    finished_at timestamp with time zone,
    CONSTRAINT webhook_deliveries_state_check CHECK ((state = ANY (ARRAY['pending'::text, 'delivered'::text, 'failed'::text])))
);


--
-- Name: webhooks; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.webhooks (
    id uuid NOT NULL,
    repo_id uuid,
    org_id uuid,
    url text NOT NULL,
    secret_enc bytea NOT NULL,
    key_id text NOT NULL,
    events text[] NOT NULL,
    active boolean DEFAULT true NOT NULL,
    disabled_reason text,
    consecutive_failures integer DEFAULT 0 NOT NULL,
    principal_id uuid,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    updated_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT webhooks_check CHECK (((repo_id IS NULL) <> (org_id IS NULL))),
    CONSTRAINT webhooks_events_check CHECK ((cardinality(events) > 0)),
    CONSTRAINT webhooks_url_check CHECK ((length(url) <= 2048))
);


--
-- Name: jobs id; Type: DEFAULT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.jobs ALTER COLUMN id SET DEFAULT nextval('public.jobs_id_seq'::regclass);


--
-- Data for Name: _sqlx_migrations; Type: TABLE DATA; Schema: public; Owner: -
--

COPY public._sqlx_migrations (version, description, installed_on, success, checksum, execution_time) FROM stdin;
100	baseline	2026-11-18 13:43:16.487363+00	t	\\x21982106074b838c30a089284f29ef557a0c54c5a2732106e77b2d2da90d0657938e64f7ca57d5796865202fd7c97eab	373248507
101	merge requests	2026-11-18 13:43:16.863633+00	t	\\x161b4fc4267b143a8d2af9e461cae7e0584fd36589bc9baa47e2847606b130c96ba4c281eb50636fd3ff14bb479dfc8e	34940863
102	push protection	2026-11-18 13:43:16.899101+00	t	\\x2012997ef055961e8e714ed20b955c1d8fda1e78e4d3cc89b22e6790c9b6224e0c635be4e8f54e59847eebd642d0d4e1	2818767
103	secret allowances	2026-11-18 13:43:16.902635+00	t	\\x1c993a79e750fde37dbe2ebcbdba84d6089d476608c469abffce4f5d68608f8e662ab270cca72c4138550f457ac42158	9275965
104	secret allowance role	2026-11-18 13:43:16.912556+00	t	\\x6b0b5a549c09e61807f88a2cfd8666b5f1b04d0c5101925931e60688f78a0106e88cfc94907ead7bed9197b3b7b25acd	3276945
105	merge request events	2026-11-18 13:43:16.916793+00	t	\\x5b50c69b4567b502f903ef303334470c77585377cf5a3d817dc8635bc60b9cfdb8cc7d0a5ed27d04d994278f5acb2a47	6083809
106	mr changes	2026-11-18 13:43:16.923939+00	t	\\x030f4c79d0fdad887d5f668ae4b92d3a7c83b2b2224673aa4e1b2791fb6511df95ab5043b38f96ac19fada6a5e4792fd	17776411
107	diff preferences	2026-11-18 13:43:16.942111+00	t	\\x2499930743f4a94d2fae37032af7f54defe7b5de83644efec4725641e8d1a351ab49bd74cebedea5206c1856d5493e78	2594032
108	mr reviews	2026-11-18 13:43:16.945311+00	t	\\x2c9b560e2cde3e624a7bc1e7679e8d0fd869fe3822285fcfd6844503c05d67c334372dfb948ee71de18f471264c260d4	8606397
109	commit statuses	2026-11-18 13:43:16.954321+00	t	\\x8c356f455941d6170f3f48a0167b98e54382939dfbfa44091e4d1c01f42d0202e9d5456c5c2cef45a7dd940fb7235391	6200401
110	mr ledger	2026-11-18 13:43:16.961053+00	t	\\x567a913bc7339ebcf96a5a539dbc5e1bbfbd2d89bb3891c95beb888839b626309475fe75c60b4b79d7dfe6291d798dbc	6002216
111	strict approval dismissal	2026-11-18 13:43:16.967905+00	t	\\xbcd9d1ba0cc84a879a584d3fa42a1ea8f741c385c13be450a70bc08b422e891b9f1bd61a23827cb3f1d6380ebd482f09	3121891
112	mr threads	2026-11-18 13:43:16.971452+00	t	\\xc2830fe455720c5b69ee35fee277f2919e9fe884d94ca4a419495970b4ed092be58dc35d9ec8536682385468c094c099	19004417
113	repo forks	2026-11-18 13:43:16.991178+00	t	\\xa0ca72aa898f305ef689735e0a58cb1077218698642f27e21109a551e6c4fe8e584950bacead977c1e68f1d5649ea553	6396472
114	locked forks	2026-11-18 13:43:16.997936+00	t	\\xac6f3d7171c899042083ec4f7bccaedcb08c44642c4eb17a116541fbccacdd88b5074f2fec5f4cc943064d3b9598cc30	2964684
115	fork merge requests	2026-11-18 13:43:17.001771+00	t	\\xdaa2029dae3bea07d676f935f319fae31ff9e08dd0c290f53ec1f23395557a0d7be8e4fa1e97d780324a63e96e075167	2916292
116	mr source findings	2026-11-18 13:43:17.005106+00	t	\\x27e2d0e45c03b834d69b207a96748f32584973c6e0d75ad5e3051f5a4de20ad162fc9aa8e65746d145dbe6f663c7ba1e	2099496
117	maintainer edit events	2026-11-18 13:43:17.008006+00	t	\\xf1501f378037602d2af821ff85e6eb736f0e5d18bee0304fd116e8aa85d4f7173e7e54efc6f3fc371035aac5f7e5b8d0	2773382
118	branch updated events	2026-11-18 13:43:17.011356+00	t	\\x81d988a6ad8718200fbd65d7caae1e5623950891091cc91143c0522f9bf492038894c226d0766d5a4f706a8a049ebe29	2922925
119	auto merge	2026-11-18 13:43:17.01465+00	t	\\x48cc7cbe480becebbfda0aa8a06f393d19dcb1f67a9f19df23439c4c64e8992510f2323cf3572088961297b00ca4606e	2943643
120	deploy keys	2026-11-18 13:43:17.018394+00	t	\\x4c885a31681749434b7d4de02cf623e9e6bbbe474a100d4d7508976be8daa57f5e3d2af4960b242a502ef47ea1d67e70	6161347
121	vcs tool	2026-11-18 13:43:17.024919+00	t	\\x4cb2a798da370ce33626c78f265276409018411e7b7135b5f076126aaca18f2a53d957c36d9fa3e19abfb76650656bd2	2729218
122	mr checkouts	2026-11-18 13:43:17.028107+00	t	\\xbdd967ef9d2ee85e895bd50609825c22fff5aa67896de44a974fbda39d2392b761610fd80684c90e6d96f2f401130c74	4620108
123	approval coverage	2026-11-18 13:43:17.033291+00	t	\\x78e445e21e5f90d12d3f3eac7bb83c50bfe5709e3986d76eadf508bc1bf174b9dd9877f3bf480b1587bef61726ea3316	6088559
124	intentional secrets	2026-11-18 13:43:17.03992+00	t	\\xf793a0c2cdbf4c1804ed1d0dabc6c3f8fea739e3a3d894b16afb01b2d7a7871ecabddc5d59a5cb0d818b43beb383564a	3144795
125	allowance descriptions	2026-11-18 13:43:17.043478+00	t	\\x626389f12c69ee56c0e2af248aba287958e4ee2b9d6cc86f3f7aae7412531ea851bdfec0d1c80b598a9a6fb3075da128	1941185
126	webhooks	2026-11-18 13:43:17.046055+00	t	\\x7ad23cef2c2b253618276cb65e1322f7c1fddd3a563a01bb328a30fa86afcc1cad4c50d86b5b7537b9087950bcf13900	22208644
127	bot owners	2026-11-18 13:43:17.068946+00	t	\\x9b31b3bd1e433ce41b5d61703d5ad1efcd3158bff51258ca5bcf8ba007981159b6c1fc845b9af3a3ec96ea8cbd0e2d11	3267468
128	status annotations	2026-11-18 13:43:17.072509+00	t	\\x83a98e735a4630a237ed199bf72df0a77a64d9afcebe7b613b2f347d9e958f7e7319062737fec1356852dfab3f65f46b	5832345
129	finding threads	2026-11-18 13:43:17.078931+00	t	\\x020278e15a99993113c61ec3e67c9124665d09fc88b5c5e0e906c78122690eda60ca929b6728b572c79da105ad49043d	7657319
130	bot stop	2026-11-18 13:43:17.087398+00	t	\\xf77f6753307806c0009ff4a93848e99d585c2f4b5597d49b65a6e00ec978413bd6f2138857f95830ec73d57bdc980666	3300721
131	bot freezes	2026-11-18 13:43:17.091198+00	t	\\x7404d69cd9eb01bf3db1ce5fba5556782efbaaac3a9fa7a73b2374c4a1bb9e819e50d15d08e4f16b08c723ecaa4b0c53	7164206
132	sensitive paths	2026-11-18 13:43:17.099195+00	t	\\x7d0de67c33a0ee626973864f560036964bdac60edc8fd6be2999d53941198214c779ca95617e9b55fe953471324f148a	6360615
133	mr attention	2026-11-18 13:43:17.105892+00	t	\\xfb33783f15d232e6bc5b74bbe561c4aba49220ab0c8c9b6d1bc14f8c28d8da89c60a748adf13ec479459d0a687642b29	4950474
134	text origins	2026-11-18 13:43:17.111269+00	t	\\x908432b78940589a7639bfc7291c6d9d018dde14026a8c34c34c275b50300b392d8c889f36d1b0521d5eca276adcd46d	24213510
135	thread addressed	2026-11-18 13:43:17.136439+00	t	\\x796a18412862040bc167c6c6d7ff34f71f0c86e727e48c77846cedc233b1d8e38a038d2a0d6e9b6fb4e251fa65bb9f45	4864672
136	user emails	2026-11-18 13:43:17.141649+00	t	\\x1c23344341349a11dec751ce88d33daf139eeb8d8e839b0c35d5b2943476a31112847ce01535c3c719a9945e00761a3c	5499033
137	bot commits	2026-11-18 13:43:17.147799+00	t	\\x95f93662bcf02f1997a90f471c7c250a6777ab245a8ed5961cf56c1ca866f9f999dd094a85f16cf9e91c67e5b053ed33	7956997
138	resolving replies	2026-11-18 13:43:17.156311+00	t	\\xe27b0c4860998c401daa9dcde0f5f32702508736ccba1b011970020e16a644ffb533f049d4763990260dd3601092acdf	2954655
139	instance logos	2026-11-18 13:43:17.159779+00	t	\\x18c68ad2e80e93f46d0f8e858e6cadfd9d234bc0b03060859d8c5ebdcaefea0921cd513c0e1ae65aed49ee05b583ee19	6118576
\.


--
-- Data for Name: access_tokens; Type: TABLE DATA; Schema: public; Owner: -
--

COPY public.access_tokens (id, user_id, name, token_prefix, token_hash, scopes, repo_ids, expires_at, last_used_at, revoked_at, created_at) FROM stdin;
01a1eee5-c5af-76ab-a662-7da6652ae86a	01a1eee5-c3ed-7195-8e19-b7367c823361	tests	Rkkj3PdN	\\x461bf8bc08ccd316625dddfeadeb2f87e13af3abd6baec9e0e7038956aaf896f	{repo:write,user:read}	\N	2026-12-18 13:43:18.19154+00	2026-11-18 13:43:19.021917+00	\N	2026-11-18 13:43:18.193904+00
01a1eee5-cbe1-7721-8a17-424ace5bdafa	01a1eee5-c6fa-7690-b280-18dab0b26714	ci	4KgNlqOi	\\x67d93553e3ac40331dac54bf0ba1ed5c83f35aafa8821540c02e61c53fe5e9ea	{repo:read}	\N	2026-12-18 13:43:19.777344+00	\N	\N	2026-11-18 13:43:19.7781+00
01a1eee5-ccc3-763a-b768-ea9644961bf4	01a1eee5-c3ed-7195-8e19-b7367c823361	fixture	Uv4HgTYT	\\x24125c58ecf3e917c59233bc967ae2a7e30e38a1dccf54d29702df0deff12156	{repo:read,issues:write}	\N	2026-12-18 13:43:20.003672+00	2026-11-18 13:43:20.022821+00	\N	2026-11-18 13:43:20.005174+00
01a1eee5-ce26-7422-88a1-9a101affced4	01a1eee5-c3ed-7195-8e19-b7367c823361	fixture-0.3	4HU8qdMH	\\x65b4258c6cad9747eb497eb61e450001a1c480e067e47d10721ea7a3f298796d	{repo:write,mrs:write,statuses:write,user:write}	\N	2026-12-18 13:43:20.358126+00	2026-11-18 13:43:20.717382+00	\N	2026-11-18 13:43:20.358882+00
01a1eee5-ce33-750b-a9d5-02de7aee565a	01a1eee5-c6fa-7690-b280-18dab0b26714	fixture-0.3	HKz9aVyE	\\xb45667b5b9f4934e1b1925940dc0809b8f72edbf68346ae44b1d36c3b022d32d	{repo:write,mrs:write}	\N	2026-12-18 13:43:20.371838+00	2026-11-18 13:43:21.031409+00	\N	2026-11-18 13:43:20.373064+00
01a1eee5-d367-726a-9634-76b8d03cd62a	01a1eee5-d334-76db-a69c-3c5bfe800e10	ci	kZtk3oxA	\\xe2161787431db579a4b515ffb39758056389d824cc805674e9120a16bebcd15b	{repo:read}	{01a1eee5-ca5d-71b1-a3cc-66b3c21ae6d8}	2026-12-18 13:43:21.703732+00	\N	\N	2026-11-18 13:43:21.704511+00
\.


--
-- Data for Name: attachments; Type: TABLE DATA; Schema: public; Owner: -
--

COPY public.attachments (id, repo_id, uploader_id, blob_sha256, filename, content_type, size_bytes, created_at) FROM stdin;
\.


--
-- Data for Name: audit_events; Type: TABLE DATA; Schema: public; Owner: -
--

COPY public.audit_events (id, actor_id, actor_ip, action, target_kind, target_id, repo_id, org_id, data, created_at) FROM stdin;
01a1eee5-c406-77e4-a326-536c7900be52	\N	\N	user.create	user	01a1eee5-c3ed-7195-8e19-b7367c823361	\N	\N	{"admin": false, "username": "alice"}	2026-11-18 13:43:17.76922+00
01a1eee5-c488-778f-96e1-fae7be60816e	01a1eee5-c3ed-7195-8e19-b7367c823361	127.0.0.1	login.success	user	01a1eee5-c3ed-7195-8e19-b7367c823361	\N	\N	{}	2026-11-18 13:43:17.898135+00
01a1eee5-c502-76b7-893f-8ee427324182	01a1eee5-c3ed-7195-8e19-b7367c823361	127.0.0.1	repo.create	repository	01a1eee5-c4a1-735f-9d08-6e6aa6f67566	01a1eee5-c4a1-735f-9d08-6e6aa6f67566	\N	{"name": "alice/demo", "visibility": "private"}	2026-11-18 13:43:18.020021+00
01a1eee5-c592-7714-a4e4-9957dcaa1483	01a1eee5-c3ed-7195-8e19-b7367c823361	127.0.0.1	login.success	user	01a1eee5-c3ed-7195-8e19-b7367c823361	\N	\N	{}	2026-11-18 13:43:18.165031+00
01a1eee5-c5b6-72bf-a0f8-d19e8a5b63a9	01a1eee5-c3ed-7195-8e19-b7367c823361	127.0.0.1	token.create	token	01a1eee5-c5af-76ab-a662-7da6652ae86a	\N	\N	{"name": "tests", "user": "01a1eee5-c3ed-7195-8e19-b7367c823361", "scopes": ["repo:write", "user:read"], "expires_at": 1797601398}	2026-11-18 13:43:18.200308+00
01a1eee5-c702-756e-8891-74cb8aab3b18	\N	\N	user.create	user	01a1eee5-c6fa-7690-b280-18dab0b26714	\N	\N	{"admin": false, "username": "bob"}	2026-11-18 13:43:18.531526+00
01a1eee5-c83d-77c4-abee-47b0aa41584c	\N	\N	user.create	user	01a1eee5-c838-7429-8a61-88c71aeb2ef0	\N	\N	{"admin": true, "username": "root-admin"}	2026-11-18 13:43:18.847328+00
01a1eee5-ca17-77da-98fe-1e8a3817508f	01a1eee5-c3ed-7195-8e19-b7367c823361	127.0.0.1	login.success	user	01a1eee5-c3ed-7195-8e19-b7367c823361	\N	\N	{}	2026-11-18 13:43:19.320037+00
01a1eee5-ca2a-712e-9729-8a714d84cd4d	01a1eee5-c3ed-7195-8e19-b7367c823361	127.0.0.1	org.create	organization	01a1eee5-ca23-72a1-a05a-24dab42879d7	\N	01a1eee5-ca23-72a1-a05a-24dab42879d7	{"name": "acme"}	2026-11-18 13:43:19.339323+00
01a1eee5-ca48-7034-947d-2bcfcce498b6	01a1eee5-c3ed-7195-8e19-b7367c823361	127.0.0.1	org.member.set	organization	01a1eee5-ca23-72a1-a05a-24dab42879d7	\N	01a1eee5-ca23-72a1-a05a-24dab42879d7	{"role": "member", "username": "bob"}	2026-11-18 13:43:19.36921+00
01a1eee5-caa0-75ec-9fdb-f49a31ce831c	01a1eee5-c3ed-7195-8e19-b7367c823361	127.0.0.1	repo.create	repository	01a1eee5-ca5d-71b1-a3cc-66b3c21ae6d8	01a1eee5-ca5d-71b1-a3cc-66b3c21ae6d8	01a1eee5-ca23-72a1-a05a-24dab42879d7	{"name": "acme/tools", "visibility": "public"}	2026-11-18 13:43:19.457089+00
01a1eee5-cac5-71ed-bf04-a3e1529b66d9	01a1eee5-c3ed-7195-8e19-b7367c823361	127.0.0.1	repo.collaborator.set	repository	01a1eee5-c4a1-735f-9d08-6e6aa6f67566	01a1eee5-c4a1-735f-9d08-6e6aa6f67566	\N	{"role": "write", "username": "bob"}	2026-11-18 13:43:19.493959+00
01a1eee5-cb16-75ff-a418-95dcfb1d8f7d	01a1eee5-c3ed-7195-8e19-b7367c823361	127.0.0.1	repo.create	repository	01a1eee5-cad5-7185-aeca-8aee6aa64a80	01a1eee5-cad5-7185-aeca-8aee6aa64a80	\N	{"name": "alice/renamed", "visibility": "private"}	2026-11-18 13:43:19.576053+00
01a1eee5-cb3b-76ef-b7ed-4e65db181b82	01a1eee5-c3ed-7195-8e19-b7367c823361	127.0.0.1	repo.rename	repository	01a1eee5-cad5-7185-aeca-8aee6aa64a80	01a1eee5-cad5-7185-aeca-8aee6aa64a80	\N	{"to": "new-name", "from": "renamed"}	2026-11-18 13:43:19.612199+00
01a1eee5-cb89-756e-b5dc-64b20435a5af	01a1eee5-c6fa-7690-b280-18dab0b26714	127.0.0.1	login.success	user	01a1eee5-c6fa-7690-b280-18dab0b26714	\N	\N	{}	2026-11-18 13:43:19.690531+00
01a1eee5-cb99-7393-986a-7d9fd409ef63	01a1eee5-c6fa-7690-b280-18dab0b26714	127.0.0.1	ssh_key.add	ssh_key	01a1eee5-cb96-71f1-89e9-6062771b3315	\N	\N	{"title": "test key", "fingerprint": "SHA256:sMJlVCYio5wL0l3yFFRe77bTryQ0r/XurKJKJ6UZnYE"}	2026-11-18 13:43:19.706557+00
01a1eee5-cbd2-701d-866a-feffda9dc0b8	01a1eee5-c6fa-7690-b280-18dab0b26714	127.0.0.1	login.success	user	01a1eee5-c6fa-7690-b280-18dab0b26714	\N	\N	{}	2026-11-18 13:43:19.762958+00
01a1eee5-cbe3-74fb-9ace-75c6435368f3	01a1eee5-c6fa-7690-b280-18dab0b26714	127.0.0.1	token.create	token	01a1eee5-cbe1-7721-8a17-424ace5bdafa	\N	\N	{"name": "ci", "user": "01a1eee5-c6fa-7690-b280-18dab0b26714", "scopes": ["repo:read"], "expires_at": 1797601399}	2026-11-18 13:43:19.780114+00
01a1eee5-cc30-7480-888b-d97670793358	01a1eee5-c3ed-7195-8e19-b7367c823361	127.0.0.1	repo.create	repository	01a1eee5-cbf3-7345-bbc7-f242f7adf4bb	01a1eee5-cbf3-7345-bbc7-f242f7adf4bb	\N	{"name": "alice/inside", "visibility": "internal"}	2026-11-18 13:43:19.857534+00
01a1eee5-cc47-73f4-ab4e-f4902562f8ff	01a1eee5-c3ed-7195-8e19-b7367c823361	127.0.0.1	team.create	organization	01a1eee5-ca23-72a1-a05a-24dab42879d7	\N	01a1eee5-ca23-72a1-a05a-24dab42879d7	{"team": "core"}	2026-11-18 13:43:19.88022+00
01a1eee5-cc64-715f-89c8-c388a0475097	01a1eee5-c3ed-7195-8e19-b7367c823361	127.0.0.1	team.member.add	organization	01a1eee5-ca23-72a1-a05a-24dab42879d7	\N	01a1eee5-ca23-72a1-a05a-24dab42879d7	{"team": "core", "username": "bob"}	2026-11-18 13:43:19.908931+00
01a1eee5-cc90-712e-b726-20e4029df298	01a1eee5-c3ed-7195-8e19-b7367c823361	127.0.0.1	repo.team.set	repository	01a1eee5-ca5d-71b1-a3cc-66b3c21ae6d8	01a1eee5-ca5d-71b1-a3cc-66b3c21ae6d8	\N	{"role": "write", "team": "core"}	2026-11-18 13:43:19.953163+00
01a1eee5-ccb9-733b-ab98-e1732458ee4f	01a1eee5-c3ed-7195-8e19-b7367c823361	127.0.0.1	protection.set	repository	01a1eee5-c4a1-735f-9d08-6e6aa6f67566	01a1eee5-c4a1-735f-9d08-6e6aa6f67566	\N	{"pattern": "main", "push_role": "maintain", "require_mr": false, "block_deletion": false, "block_force_push": true, "status_reporters": {}, "require_up_to_date": false, "required_approvals": 1, "bot_merge_allowlist": [], "statuses_on_head_ok": false, "ready_waits_for_checks": false, "bots_count_as_approvers": false, "dismiss_stale_approvals": false, "require_resolved_threads": false, "required_status_contexts": [], "dismiss_approvals_on_new_revision": false}	2026-11-18 13:43:19.994171+00
01a1eee5-ccc6-7688-8226-2a4550b82cce	01a1eee5-c3ed-7195-8e19-b7367c823361	127.0.0.1	token.create	token	01a1eee5-ccc3-763a-b768-ea9644961bf4	\N	\N	{"name": "fixture", "user": "01a1eee5-c3ed-7195-8e19-b7367c823361", "scopes": ["repo:read", "issues:write"], "expires_at": 1797601400}	2026-11-18 13:43:20.007627+00
01a1eee5-ce27-750a-81da-9509a3c417db	01a1eee5-c3ed-7195-8e19-b7367c823361	127.0.0.1	token.create	token	01a1eee5-ce26-7422-88a1-9a101affced4	\N	\N	{"name": "fixture-0.3", "user": "01a1eee5-c3ed-7195-8e19-b7367c823361", "scopes": ["repo:write", "mrs:write", "statuses:write", "user:write"], "expires_at": 1797601400}	2026-11-18 13:43:20.360638+00
01a1eee5-ce35-76c5-b297-dfb5b75a0b43	01a1eee5-c6fa-7690-b280-18dab0b26714	127.0.0.1	token.create	token	01a1eee5-ce33-750b-a9d5-02de7aee565a	\N	\N	{"name": "fixture-0.3", "user": "01a1eee5-c6fa-7690-b280-18dab0b26714", "scopes": ["repo:write", "mrs:write"], "expires_at": 1797601400}	2026-11-18 13:43:20.37443+00
01a1eee5-cffe-7101-b038-4301a7910b6a	01a1eee5-c3ed-7195-8e19-b7367c823361	127.0.0.1	mr.create	merge_request	01a1eee5-cf9c-765c-a36f-360b5b0c1b31	01a1eee5-c4a1-735f-9d08-6e6aa6f67566	\N	{"number": 3, "source_repo": null}	2026-11-18 13:43:20.83134+00
01a1eee5-d098-7505-939b-1b0f511b4b4a	01a1eee5-c3ed-7195-8e19-b7367c823361	127.0.0.1	mr.create	merge_request	01a1eee5-d03e-7509-b00f-1b19eaad6b81	01a1eee5-c4a1-735f-9d08-6e6aa6f67566	\N	{"number": 4, "source_repo": null}	2026-11-18 13:43:20.984839+00
01a1eee5-d196-704a-b553-34d43c05d184	01a1eee5-c3ed-7195-8e19-b7367c823361	127.0.0.1	mr.merge	merge_request	01a1eee5-cf9c-765c-a36f-360b5b0c1b31	01a1eee5-c4a1-735f-9d08-6e6aa6f67566	\N	{"oid": "9f96126a44821776bcb75c489267994abef4dc85", "number": 3, "strategy": "merge-commit"}	2026-11-18 13:43:21.239863+00
01a1eee5-d2ea-7784-bcef-95998c9ba1a3	01a1eee5-c6fa-7690-b280-18dab0b26714	127.0.0.1	repo.create	repository	01a1eee5-d2a8-7404-a045-392ebd0a669f	01a1eee5-d2a8-7404-a045-392ebd0a669f	\N	{"name": "bob/tools", "visibility": "public"}	2026-11-18 13:43:21.579353+00
01a1eee5-d2ed-75f2-a73f-92d42a322ac2	01a1eee5-c6fa-7690-b280-18dab0b26714	127.0.0.1	repo.fork	repository	01a1eee5-d2a8-7404-a045-392ebd0a669f	01a1eee5-d2a8-7404-a045-392ebd0a669f	\N	{"parent": "acme/tools", "default_branch_only": false}	2026-11-18 13:43:21.582586+00
01a1eee5-d304-7671-ac58-9c0339587494	01a1eee5-c3ed-7195-8e19-b7367c823361	127.0.0.1	webhook.create	repository	01a1eee5-c4a1-735f-9d08-6e6aa6f67566	01a1eee5-c4a1-735f-9d08-6e6aa6f67566	\N	{"url": "http://127.0.0.1:9/hook", "events": ["push", "merge_request"], "webhook": "01a1eee5-d2fd-72e5-8762-b811e3ce3c7b"}	2026-11-18 13:43:21.604853+00
01a1eee5-d319-70c9-9cd8-aa54c1a35978	01a1eee5-c3ed-7195-8e19-b7367c823361	127.0.0.1	deploy_key.add	repository	01a1eee5-c4a1-735f-9d08-6e6aa6f67566	01a1eee5-c4a1-735f-9d08-6e6aa6f67566	\N	{"key": "01a1eee5-d316-700c-8c9a-5df9513d562d", "title": "deploy", "can_push": false, "fingerprint": "SHA256:hcbw8vHb0kmeCAIz4XZxW/xIXPZbOzVB0n1K5zwdFoo"}	2026-11-18 13:43:21.625528+00
01a1eee5-d328-7191-a14d-5942af8cd9aa	01a1eee5-c3ed-7195-8e19-b7367c823361	127.0.0.1	repo.sensitive_paths	repository	01a1eee5-c4a1-735f-9d08-6e6aa6f67566	01a1eee5-c4a1-735f-9d08-6e6aa6f67566	\N	{"paths": ["deploy/**"]}	2026-11-18 13:43:21.641691+00
01a1eee5-d33c-72cb-be98-ef34f5012d4b	01a1eee5-c3ed-7195-8e19-b7367c823361	127.0.0.1	bot.create	user	01a1eee5-d334-76db-a69c-3c5bfe800e10	\N	\N	{"name": "acme-ci", "owner": "acme"}	2026-11-18 13:43:21.660988+00
01a1eee5-d369-7007-a4c5-1e6b1fe7d3a8	01a1eee5-c3ed-7195-8e19-b7367c823361	127.0.0.1	token.create	token	01a1eee5-d367-726a-9634-76b8d03cd62a	\N	\N	{"name": "ci", "user": "01a1eee5-d334-76db-a69c-3c5bfe800e10", "scopes": ["repo:read"], "expires_at": 1797601401}	2026-11-18 13:43:21.705911+00
\.


--
-- Data for Name: background_migrations; Type: TABLE DATA; Schema: public; Owner: -
--

COPY public.background_migrations (name, introduced_in, state, cursor, processed, total_estimate, last_error, started_at, finished_at, updated_at) FROM stdin;
repo_config_v1	0.2.0	done	\N	0	0	\N	2026-11-18 13:43:17.373538+00	2026-11-18 13:43:17.373538+00	2026-11-18 13:43:17.373538+00
\.


--
-- Data for Name: blobs; Type: TABLE DATA; Schema: public; Owner: -
--

COPY public.blobs (sha256, size_bytes, refcount, unreferenced_since, created_at) FROM stdin;
\.


--
-- Data for Name: bot_commits; Type: TABLE DATA; Schema: public; Owner: -
--

COPY public.bot_commits (network_id, oid, bot_id, created_at) FROM stdin;
\.


--
-- Data for Name: bot_freezes; Type: TABLE DATA; Schema: public; Owner: -
--

COPY public.bot_freezes (org_id, frozen_by, frozen_at) FROM stdin;
\.


--
-- Data for Name: branch_protections; Type: TABLE DATA; Schema: public; Owner: -
--

COPY public.branch_protections (id, repo_id, pattern, block_force_push, block_deletion, require_mr, required_approvals, required_status_contexts, push_role, dismiss_stale_approvals, statuses_on_head_ok, require_resolved_threads, bots_count_as_approvers, bot_merge_allowlist, require_up_to_date, created_at, status_reporters, dismiss_approvals_on_new_revision, ready_waits_for_checks) FROM stdin;
01a1eee5-ccb6-7456-8b99-67475a55aca5	01a1eee5-c4a1-735f-9d08-6e6aa6f67566	main	t	f	f	1	{}	maintain	f	f	f	f	{}	f	2026-11-18 13:43:19.991724+00	{}	f	f
\.


--
-- Data for Name: comments; Type: TABLE DATA; Schema: public; Owner: -
--

COPY public.comments (id, repo_id, subject_kind, subject_id, author_id, body, edited_at, deleted_at, hidden_at, created_at, origin) FROM stdin;
01a1eee5-ce06-7282-943c-93c0b0487f9e	01a1eee5-c4a1-735f-9d08-6e6aa6f67566	issue	01a1eee5-ccf7-72f6-95d0-80259c33e486	01a1eee5-c6fa-7690-b280-18dab0b26714	On it, thanks @alice. See #2.	\N	\N	\N	2026-11-18 13:43:20.32784+00	writer
\.


--
-- Data for Name: commit_statuses; Type: TABLE DATA; Schema: public; Owner: -
--

COPY public.commit_statuses (id, repo_id, commit_oid, context, state, description, target_url, creator_kind, creator_id, created_at, origin) FROM stdin;
01a1eee5-d28d-705c-88eb-515c0a1a4710	01a1eee5-c4a1-735f-9d08-6e6aa6f67566	01cca1adb1719e6fe29ff4b58a7ad0b133d24292	ci/build	success	Build passed	\N	user	01a1eee5-c3ed-7195-8e19-b7367c823361	2026-11-18 13:43:21.484985+00	writer
\.


--
-- Data for Name: email_verifications; Type: TABLE DATA; Schema: public; Owner: -
--

COPY public.email_verifications (token_hash, user_id, email, expires_at, used_at, created_at) FROM stdin;
\.


--
-- Data for Name: encryption_keys; Type: TABLE DATA; Schema: public; Owner: -
--

COPY public.encryption_keys (key_id, created_at, retired_at) FROM stdin;
7d3b01b9	2026-11-18 13:43:17.180567+00	\N
\.


--
-- Data for Name: git_refs; Type: TABLE DATA; Schema: public; Owner: -
--

COPY public.git_refs (repo_id, name, oid, updated_at) FROM stdin;
01a1eee5-c4a1-735f-9d08-6e6aa6f67566	\\x726566732f746167732f72656c656173652f322e30	822b6c5fab6d985b4dbf8caccf6ca5db811ab916	2026-11-18 13:43:19.211645+00
01a1eee5-c4a1-735f-9d08-6e6aa6f67566	\\x726566732f746167732f747265652d746167	b952f8c74fe4b4f428492d75c1e227657be2bf48	2026-11-18 13:43:19.211645+00
01a1eee5-c4a1-735f-9d08-6e6aa6f67566	\\x726566732f746167732f76312e30	032aee9a437815204df8a21bbc7371dcec2bd0e3	2026-11-18 13:43:19.211645+00
01a1eee5-c4a1-735f-9d08-6e6aa6f67566	\\x726566732f746167732f76312e302d6c69676874	3f3ec41f7a4fefa97602911c0258528cc2ad3d25	2026-11-18 13:43:19.211645+00
01a1eee5-c4a1-735f-9d08-6e6aa6f67566	\\x726566732f746167732f76322e302d6d657461	e1e0b1fbb57d0584f431150d048088a9e1508dbe	2026-11-18 13:43:19.211645+00
01a1eee5-c4a1-735f-9d08-6e6aa6f67566	\\x726566732f68656164732f6669782d61	01cca1adb1719e6fe29ff4b58a7ad0b133d24292	2026-11-18 13:43:20.682632+00
01a1eee5-c4a1-735f-9d08-6e6aa6f67566	\\x726566732f68656164732f6d61696e	9f96126a44821776bcb75c489267994abef4dc85	2026-11-18 13:43:21.204565+00
\.


--
-- Data for Name: idempotency_keys; Type: TABLE DATA; Schema: public; Owner: -
--

COPY public.idempotency_keys (principal_id, key, method, path, request_sha256, status, response, created_at) FROM stdin;
\.


--
-- Data for Name: instance_logos; Type: TABLE DATA; Schema: public; Owner: -
--

COPY public.instance_logos (theme, blob_sha256, content_type, updated_by, updated_at) FROM stdin;
\.


--
-- Data for Name: instance_settings; Type: TABLE DATA; Schema: public; Owner: -
--

COPY public.instance_settings (key, value, updated_at) FROM stdin;
served_by_version	"0.3.0-rc.1"	2026-11-18 13:43:17.328793+00
\.


--
-- Data for Name: invitations; Type: TABLE DATA; Schema: public; Owner: -
--

COPY public.invitations (id, token_hash, created_by, email, note, expires_at, used_at, used_by, revoked_at, created_at) FROM stdin;
\.


--
-- Data for Name: issue_assignees; Type: TABLE DATA; Schema: public; Owner: -
--

COPY public.issue_assignees (issue_id, user_id) FROM stdin;
01a1eee5-ccf7-72f6-95d0-80259c33e486	01a1eee5-c6fa-7690-b280-18dab0b26714
\.


--
-- Data for Name: issue_labels; Type: TABLE DATA; Schema: public; Owner: -
--

COPY public.issue_labels (issue_id, label_id) FROM stdin;
01a1eee5-ccf7-72f6-95d0-80259c33e486	01a1eee5-c4ac-766b-92e8-65901dfc0971
01a1eee5-ccf7-72f6-95d0-80259c33e486	01a1eee5-c4b4-702e-a41c-7fd14615dbb3
\.


--
-- Data for Name: issues; Type: TABLE DATA; Schema: public; Owner: -
--

COPY public.issues (id, repo_id, number, author_id, title, body, state, state_reason, milestone_id, closed_at, created_at, updated_at, origin) FROM stdin;
01a1eee5-cd91-717a-a190-339a2fb7dcef	01a1eee5-c4a1-735f-9d08-6e6aa6f67566	2	01a1eee5-c3ed-7195-8e19-b7367c823361	Document the config file		closed	completed	\N	2026-11-18 13:43:20.245751+00	2026-11-18 13:43:20.207954+00	2026-11-18 13:43:20.245751+00	writer
01a1eee5-ccf7-72f6-95d0-80259c33e486	01a1eee5-c4a1-735f-9d08-6e6aa6f67566	1	01a1eee5-c3ed-7195-8e19-b7367c823361	Crash when the config is empty	Steps:\n\n1. Start with an empty file.\n\n@bob, can you look?	open	\N	01a1eee5-ccde-766c-90b9-58acc3941b22	\N	2026-11-18 13:43:20.054027+00	2026-11-18 13:43:20.331017+00	writer
\.


--
-- Data for Name: jobs; Type: TABLE DATA; Schema: public; Owner: -
--

COPY public.jobs (id, queue, kind, payload, dedupe_key, state, run_at, attempts, max_attempts, locked_by, locked_until, last_error, created_at, finished_at) FROM stdin;
1	default	ref.updated	{"push_id": "01a1eee5-c9ab-7032-b92a-64611ac32f2d", "repo_id": "01a1eee5-c4a1-735f-9d08-6e6aa6f67566"}	ref.updated:01a1eee5-c9ab-7032-b92a-64611ac32f2d	done	2026-11-18 13:43:19.211645+00	1	8	\N	\N	\N	2026-11-18 13:43:19.211645+00	2026-11-18 13:43:19.275496+00
2	default	fork.access_review	{"repo_id": "01a1eee5-c4a1-735f-9d08-6e6aa6f67566"}	fork.access_review:01a1eee5-c4a1-735f-9d08-6e6aa6f67566:01a1eee5-cac6-77ad-a72c-199efc345c36	done	2026-11-18 13:43:19.495777+00	1	8	\N	\N	\N	2026-11-18 13:43:19.495777+00	2026-11-18 13:43:19.504779+00
3	default	fork.access_review	{"repo_id": "01a1eee5-ca5d-71b1-a3cc-66b3c21ae6d8"}	fork.access_review:01a1eee5-ca5d-71b1-a3cc-66b3c21ae6d8:01a1eee5-cc91-7419-99ab-d881fcc62f94	done	2026-11-18 13:43:19.95414+00	1	8	\N	\N	\N	2026-11-18 13:43:19.95414+00	2026-11-18 13:43:19.96192+00
4	default	notify.fanout	{"repo_id": "01a1eee5-c4a1-735f-9d08-6e6aa6f67566", "activity": "opened", "actor_id": "01a1eee5-c3ed-7195-8e19-b7367c823361", "assigned": [], "issue_id": "01a1eee5-ccf7-72f6-95d0-80259c33e486", "comment_id": null}	notify.fanout:01a1eee5-cd0d-7665-b48f-23936e6f485f	done	2026-11-18 13:43:20.078508+00	1	8	\N	\N	\N	2026-11-18 13:43:20.078508+00	2026-11-18 13:43:20.119188+00
5	default	notify.fanout	{"repo_id": "01a1eee5-c4a1-735f-9d08-6e6aa6f67566", "activity": "assigned", "actor_id": "01a1eee5-c3ed-7195-8e19-b7367c823361", "assigned": ["01a1eee5-c6fa-7690-b280-18dab0b26714"], "issue_id": "01a1eee5-ccf7-72f6-95d0-80259c33e486", "comment_id": null}	notify.fanout:01a1eee5-cd58-71d3-b3f2-3327af8d422f	done	2026-11-18 13:43:20.153429+00	1	8	\N	\N	\N	2026-11-18 13:43:20.153429+00	2026-11-18 13:43:20.187545+00
6	default	notify.fanout	{"repo_id": "01a1eee5-c4a1-735f-9d08-6e6aa6f67566", "activity": "opened", "actor_id": "01a1eee5-c3ed-7195-8e19-b7367c823361", "assigned": [], "issue_id": "01a1eee5-cd91-717a-a190-339a2fb7dcef", "comment_id": null}	notify.fanout:01a1eee5-cd9a-73a6-9a71-e634429f5918	done	2026-11-18 13:43:20.219644+00	1	8	\N	\N	\N	2026-11-18 13:43:20.219644+00	2026-11-18 13:43:20.246953+00
7	default	notify.fanout	{"repo_id": "01a1eee5-c4a1-735f-9d08-6e6aa6f67566", "activity": "closed", "actor_id": "01a1eee5-c3ed-7195-8e19-b7367c823361", "assigned": [], "issue_id": "01a1eee5-cd91-717a-a190-339a2fb7dcef", "comment_id": null}	notify.fanout:01a1eee5-cdbb-75e2-bc82-d6e31dc073e4	done	2026-11-18 13:43:20.252121+00	1	8	\N	\N	\N	2026-11-18 13:43:20.252121+00	2026-11-18 13:43:20.26949+00
8	default	notify.fanout	{"repo_id": "01a1eee5-c4a1-735f-9d08-6e6aa6f67566", "activity": "commented", "actor_id": "01a1eee5-c6fa-7690-b280-18dab0b26714", "assigned": [], "issue_id": "01a1eee5-ccf7-72f6-95d0-80259c33e486", "comment_id": "01a1eee5-ce06-7282-943c-93c0b0487f9e"}	notify.fanout:01a1eee5-ce17-76d8-8dda-3e039668d032	done	2026-11-18 13:43:20.344346+00	1	8	\N	\N	\N	2026-11-18 13:43:20.344346+00	2026-11-18 13:43:20.379973+00
9	default	ref.updated	{"push_id": "01a1eee5-cebb-74c1-b320-a3166415c4dc", "repo_id": "01a1eee5-c4a1-735f-9d08-6e6aa6f67566"}	ref.updated:01a1eee5-cebb-74c1-b320-a3166415c4dc	done	2026-11-18 13:43:20.507613+00	1	8	\N	\N	\N	2026-11-18 13:43:20.507613+00	2026-11-18 13:43:20.54753+00
10	default	ref.updated	{"push_id": "01a1eee5-cf6a-73db-9693-c74b4ed5892e", "repo_id": "01a1eee5-c4a1-735f-9d08-6e6aa6f67566"}	ref.updated:01a1eee5-cf6a-73db-9693-c74b4ed5892e	done	2026-11-18 13:43:20.682632+00	1	8	\N	\N	\N	2026-11-18 13:43:20.682632+00	2026-11-18 13:43:20.721192+00
11	default	notify.fanout	{"kind": "merge_request", "mr_id": "01a1eee5-cf9c-765c-a36f-360b5b0c1b31", "users": [], "item_id": null, "outcome": null, "repo_id": "01a1eee5-c4a1-735f-9d08-6e6aa6f67566", "activity": "opened", "actor_id": "01a1eee5-c3ed-7195-8e19-b7367c823361", "revision": null}	notify.fanout:01a1eee5-d00a-76e4-9cf1-3f64467a1317	done	2026-11-18 13:43:20.843339+00	1	8	\N	\N	\N	2026-11-18 13:43:20.843339+00	2026-11-18 13:43:20.865087+00
12	default	notify.fanout	{"kind": "merge_request", "mr_id": "01a1eee5-d03e-7509-b00f-1b19eaad6b81", "users": [], "item_id": null, "outcome": null, "repo_id": "01a1eee5-c4a1-735f-9d08-6e6aa6f67566", "activity": "opened", "actor_id": "01a1eee5-c3ed-7195-8e19-b7367c823361", "revision": null}	notify.fanout:01a1eee5-d0a5-7367-b343-c4c2b7dc2016	done	2026-11-18 13:43:20.998745+00	1	8	\N	\N	\N	2026-11-18 13:43:20.998745+00	2026-11-18 13:43:21.015334+00
13	default	notify.fanout	{"kind": "merge_request", "mr_id": "01a1eee5-cf9c-765c-a36f-360b5b0c1b31", "users": [], "item_id": "01a1eee5-d0ea-72b2-8cfb-1c5a21bf19af", "outcome": null, "repo_id": "01a1eee5-c4a1-735f-9d08-6e6aa6f67566", "activity": "reviewed", "actor_id": "01a1eee5-c6fa-7690-b280-18dab0b26714", "revision": null}	notify.fanout:01a1eee5-d0fe-7074-a61d-5e80a792d5c9	done	2026-11-18 13:43:21.088659+00	1	8	\N	\N	\N	2026-11-18 13:43:21.088659+00	2026-11-18 13:43:21.118569+00
15	default	ref.updated	{"push_id": "01a1eee5-d184-77d3-9ad5-8bcba0eb6e72", "repo_id": "01a1eee5-c4a1-735f-9d08-6e6aa6f67566"}	ref.updated:01a1eee5-d184-77d3-9ad5-8bcba0eb6e72	done	2026-11-18 13:43:21.220525+00	1	8	\N	\N	\N	2026-11-18 13:43:21.220525+00	2026-11-18 13:43:21.248658+00
14	default	ref.updated	{"push_id": "01a1eee5-d174-70d4-ae12-c8a8ff9d41e2", "repo_id": "01a1eee5-c4a1-735f-9d08-6e6aa6f67566"}	ref.updated:01a1eee5-d174-70d4-ae12-c8a8ff9d41e2	done	2026-11-18 13:43:21.204565+00	1	8	\N	\N	\N	2026-11-18 13:43:21.204565+00	2026-11-18 13:43:21.250345+00
16	default	notify.fanout	{"kind": "merge_request", "mr_id": "01a1eee5-cf9c-765c-a36f-360b5b0c1b31", "users": [], "item_id": null, "outcome": null, "repo_id": "01a1eee5-c4a1-735f-9d08-6e6aa6f67566", "activity": "merged", "actor_id": "01a1eee5-c3ed-7195-8e19-b7367c823361", "revision": null}	notify.fanout:01a1eee5-d194-701a-89ce-78dcf9766191	done	2026-11-18 13:43:21.237812+00	1	8	\N	\N	\N	2026-11-18 13:43:21.237812+00	2026-11-18 13:43:21.261514+00
17	default	notify.fanout	{"kind": "merge_request", "mr_id": "01a1eee5-d03e-7509-b00f-1b19eaad6b81", "users": [], "item_id": "01a1eee5-d1c6-7575-9224-b94e38962602", "outcome": null, "repo_id": "01a1eee5-c4a1-735f-9d08-6e6aa6f67566", "activity": "thread_comment", "actor_id": "01a1eee5-c6fa-7690-b280-18dab0b26714", "revision": null}	notify.fanout:01a1eee5-d1d1-75a5-9d84-b0c2bd6b258e	done	2026-11-18 13:43:21.297899+00	1	8	\N	\N	\N	2026-11-18 13:43:21.297899+00	2026-11-18 13:43:21.326597+00
18	default	notify.fanout	{"kind": "merge_request", "mr_id": "01a1eee5-d03e-7509-b00f-1b19eaad6b81", "users": [], "item_id": "01a1eee5-d1f9-741c-8d50-2a1f9a3aee63", "outcome": null, "repo_id": "01a1eee5-c4a1-735f-9d08-6e6aa6f67566", "activity": "thread_comment", "actor_id": "01a1eee5-c3ed-7195-8e19-b7367c823361", "revision": null}	notify.fanout:01a1eee5-d202-7170-889b-2d35d7f59e1b	done	2026-11-18 13:43:21.347539+00	1	8	\N	\N	\N	2026-11-18 13:43:21.347539+00	2026-11-18 13:43:21.379601+00
19	default	notify.fanout	{"kind": "merge_request", "mr_id": "01a1eee5-d03e-7509-b00f-1b19eaad6b81", "users": ["01a1eee5-c6fa-7690-b280-18dab0b26714"], "item_id": null, "outcome": null, "repo_id": "01a1eee5-c4a1-735f-9d08-6e6aa6f67566", "activity": "review_requested", "actor_id": "01a1eee5-c3ed-7195-8e19-b7367c823361", "revision": null}	notify.fanout:01a1eee5-d221-70c1-bd52-7bf4d17d6dd1	done	2026-11-18 13:43:21.379388+00	1	8	\N	\N	\N	2026-11-18 13:43:21.379388+00	2026-11-18 13:43:21.399215+00
20	default	notify.fanout	{"kind": "merge_request", "mr_id": "01a1eee5-d03e-7509-b00f-1b19eaad6b81", "users": [], "item_id": "01a1eee5-d25a-756f-ba0a-ad70ceeb79fc", "outcome": null, "repo_id": "01a1eee5-c4a1-735f-9d08-6e6aa6f67566", "activity": "reviewed", "actor_id": "01a1eee5-c6fa-7690-b280-18dab0b26714", "revision": null}	notify.fanout:01a1eee5-d269-7402-806d-fdb8531694c9	done	2026-11-18 13:43:21.449605+00	1	8	\N	\N	\N	2026-11-18 13:43:21.449605+00	2026-11-18 13:43:21.478703+00
21	default	repo.fork	{"repo_id": "01a1eee5-d2a8-7404-a045-392ebd0a669f"}	repo.fork:01a1eee5-d2a8-7404-a045-392ebd0a669f	done	2026-11-18 13:43:21.512413+00	1	8	\N	\N	\N	2026-11-18 13:43:21.512413+00	2026-11-18 13:43:21.655089+00
\.


--
-- Data for Name: labels; Type: TABLE DATA; Schema: public; Owner: -
--

COPY public.labels (id, repo_id, name, color, description, created_at, origin) FROM stdin;
01a1eee5-c4ac-766b-92e8-65901dfc0971	01a1eee5-c4a1-735f-9d08-6e6aa6f67566	bug	d73a4a	Something isn't working	2026-11-18 13:43:17.922701+00	writer
01a1eee5-c4b0-7778-a6b1-71c70f170e33	01a1eee5-c4a1-735f-9d08-6e6aa6f67566	enhancement	a2eeef	A new feature or an improvement	2026-11-18 13:43:17.922701+00	writer
01a1eee5-c4b1-70cd-a593-82269cce52ce	01a1eee5-c4a1-735f-9d08-6e6aa6f67566	documentation	0075ca	Changes to the documentation	2026-11-18 13:43:17.922701+00	writer
01a1eee5-c4b2-77b1-a5b6-ea63e0acce0e	01a1eee5-c4a1-735f-9d08-6e6aa6f67566	question	d876e3	Needs more information	2026-11-18 13:43:17.922701+00	writer
01a1eee5-c4b4-702e-a41c-7fd14615dbb3	01a1eee5-c4a1-735f-9d08-6e6aa6f67566	good first issue	7057ff	Good for newcomers	2026-11-18 13:43:17.922701+00	writer
01a1eee5-c4b5-7104-a3dc-8ab26676bf1f	01a1eee5-c4a1-735f-9d08-6e6aa6f67566	help wanted	008672	Extra attention is welcome	2026-11-18 13:43:17.922701+00	writer
01a1eee5-c4b6-71e8-ad23-c66c205f78d5	01a1eee5-c4a1-735f-9d08-6e6aa6f67566	duplicate	cfd3d7	This issue already exists	2026-11-18 13:43:17.922701+00	writer
01a1eee5-c4b6-71e8-ad23-c66d607855c6	01a1eee5-c4a1-735f-9d08-6e6aa6f67566	wontfix	ffffff	This will not be worked on	2026-11-18 13:43:17.922701+00	writer
01a1eee5-ca64-7689-a26e-3cedf7156172	01a1eee5-ca5d-71b1-a3cc-66b3c21ae6d8	bug	d73a4a	Something isn't working	2026-11-18 13:43:19.390268+00	writer
01a1eee5-ca66-7277-9de6-d3649073e2c1	01a1eee5-ca5d-71b1-a3cc-66b3c21ae6d8	enhancement	a2eeef	A new feature or an improvement	2026-11-18 13:43:19.390268+00	writer
01a1eee5-ca67-72b7-ab89-89d0bcdc4b8c	01a1eee5-ca5d-71b1-a3cc-66b3c21ae6d8	documentation	0075ca	Changes to the documentation	2026-11-18 13:43:19.390268+00	writer
01a1eee5-ca67-72b7-ab89-89d1b2821e17	01a1eee5-ca5d-71b1-a3cc-66b3c21ae6d8	question	d876e3	Needs more information	2026-11-18 13:43:19.390268+00	writer
01a1eee5-ca68-74d8-a975-eb6f3640863e	01a1eee5-ca5d-71b1-a3cc-66b3c21ae6d8	good first issue	7057ff	Good for newcomers	2026-11-18 13:43:19.390268+00	writer
01a1eee5-ca69-73cc-a991-b5c604beb004	01a1eee5-ca5d-71b1-a3cc-66b3c21ae6d8	help wanted	008672	Extra attention is welcome	2026-11-18 13:43:19.390268+00	writer
01a1eee5-ca6a-7783-b9ca-e56378386888	01a1eee5-ca5d-71b1-a3cc-66b3c21ae6d8	duplicate	cfd3d7	This issue already exists	2026-11-18 13:43:19.390268+00	writer
01a1eee5-ca6b-773e-8e65-e3a1d2a43fcc	01a1eee5-ca5d-71b1-a3cc-66b3c21ae6d8	wontfix	ffffff	This will not be worked on	2026-11-18 13:43:19.390268+00	writer
01a1eee5-cad7-774c-a93b-ea3a03b94688	01a1eee5-cad5-7185-aeca-8aee6aa64a80	bug	d73a4a	Something isn't working	2026-11-18 13:43:19.509315+00	writer
01a1eee5-cad9-71a1-b1e8-3668cbe2f4aa	01a1eee5-cad5-7185-aeca-8aee6aa64a80	enhancement	a2eeef	A new feature or an improvement	2026-11-18 13:43:19.509315+00	writer
01a1eee5-cada-7389-a26c-6322190eb07c	01a1eee5-cad5-7185-aeca-8aee6aa64a80	documentation	0075ca	Changes to the documentation	2026-11-18 13:43:19.509315+00	writer
01a1eee5-cada-7389-a26c-6323c9f8ad93	01a1eee5-cad5-7185-aeca-8aee6aa64a80	question	d876e3	Needs more information	2026-11-18 13:43:19.509315+00	writer
01a1eee5-cadb-72d5-80bb-d05363da1f27	01a1eee5-cad5-7185-aeca-8aee6aa64a80	good first issue	7057ff	Good for newcomers	2026-11-18 13:43:19.509315+00	writer
01a1eee5-cadb-72d5-80bb-d054422cdaa6	01a1eee5-cad5-7185-aeca-8aee6aa64a80	help wanted	008672	Extra attention is welcome	2026-11-18 13:43:19.509315+00	writer
01a1eee5-cadc-7558-8d34-d10924f5457d	01a1eee5-cad5-7185-aeca-8aee6aa64a80	duplicate	cfd3d7	This issue already exists	2026-11-18 13:43:19.509315+00	writer
01a1eee5-cadd-77a3-96e2-0950d550178f	01a1eee5-cad5-7185-aeca-8aee6aa64a80	wontfix	ffffff	This will not be worked on	2026-11-18 13:43:19.509315+00	writer
01a1eee5-cbf6-75af-af2c-1c3b6c83b757	01a1eee5-cbf3-7345-bbc7-f242f7adf4bb	bug	d73a4a	Something isn't working	2026-11-18 13:43:19.795918+00	writer
01a1eee5-cbf7-739f-ab36-1308fc92afa5	01a1eee5-cbf3-7345-bbc7-f242f7adf4bb	enhancement	a2eeef	A new feature or an improvement	2026-11-18 13:43:19.795918+00	writer
01a1eee5-cbf8-712e-84b8-7505ccac3ea1	01a1eee5-cbf3-7345-bbc7-f242f7adf4bb	documentation	0075ca	Changes to the documentation	2026-11-18 13:43:19.795918+00	writer
01a1eee5-cbf9-7743-9657-8bea6bd36242	01a1eee5-cbf3-7345-bbc7-f242f7adf4bb	question	d876e3	Needs more information	2026-11-18 13:43:19.795918+00	writer
01a1eee5-cbf9-7743-9657-8beb98de5322	01a1eee5-cbf3-7345-bbc7-f242f7adf4bb	good first issue	7057ff	Good for newcomers	2026-11-18 13:43:19.795918+00	writer
01a1eee5-cbfa-76ca-9b10-cbbdaeba7cff	01a1eee5-cbf3-7345-bbc7-f242f7adf4bb	help wanted	008672	Extra attention is welcome	2026-11-18 13:43:19.795918+00	writer
01a1eee5-cbfb-7231-a46d-b7530d044ff1	01a1eee5-cbf3-7345-bbc7-f242f7adf4bb	duplicate	cfd3d7	This issue already exists	2026-11-18 13:43:19.795918+00	writer
01a1eee5-cbfb-7231-a46d-b75434e8cfaa	01a1eee5-cbf3-7345-bbc7-f242f7adf4bb	wontfix	ffffff	This will not be worked on	2026-11-18 13:43:19.795918+00	writer
01a1eee5-d2aa-748c-956f-d87c8e8c8b04	01a1eee5-d2a8-7404-a045-392ebd0a669f	bug	d73a4a	Something isn't working	2026-11-18 13:43:21.512413+00	writer
01a1eee5-d2aa-748c-956f-d87d49030b70	01a1eee5-d2a8-7404-a045-392ebd0a669f	enhancement	a2eeef	A new feature or an improvement	2026-11-18 13:43:21.512413+00	writer
01a1eee5-d2ab-7594-99ad-f79338eb0457	01a1eee5-d2a8-7404-a045-392ebd0a669f	documentation	0075ca	Changes to the documentation	2026-11-18 13:43:21.512413+00	writer
01a1eee5-d2ab-7594-99ad-f7943074234e	01a1eee5-d2a8-7404-a045-392ebd0a669f	question	d876e3	Needs more information	2026-11-18 13:43:21.512413+00	writer
01a1eee5-d2ac-7035-8728-b69d4de60332	01a1eee5-d2a8-7404-a045-392ebd0a669f	good first issue	7057ff	Good for newcomers	2026-11-18 13:43:21.512413+00	writer
01a1eee5-d2ad-7526-a68e-acb2c8f517a7	01a1eee5-d2a8-7404-a045-392ebd0a669f	help wanted	008672	Extra attention is welcome	2026-11-18 13:43:21.512413+00	writer
01a1eee5-d2ad-7526-a68e-acb349d044b0	01a1eee5-d2a8-7404-a045-392ebd0a669f	duplicate	cfd3d7	This issue already exists	2026-11-18 13:43:21.512413+00	writer
01a1eee5-d2ae-7232-9978-5177be583ed0	01a1eee5-d2a8-7404-a045-392ebd0a669f	wontfix	ffffff	This will not be worked on	2026-11-18 13:43:21.512413+00	writer
\.


--
-- Data for Name: login_attempts; Type: TABLE DATA; Schema: public; Owner: -
--

COPY public.login_attempts (key, window_start, count) FROM stdin;
\.


--
-- Data for Name: login_challenges; Type: TABLE DATA; Schema: public; Owner: -
--

COPY public.login_challenges (id_hash, user_id, purpose, first_factor, data, failures, expires_at, created_at) FROM stdin;
\.


--
-- Data for Name: merge_requests; Type: TABLE DATA; Schema: public; Owner: -
--

COPY public.merge_requests (id, repo_id, number, author_id, title, description, draft, target_branch, source_repo_id, source_ref, allow_maintainer_edits, state, milestone_id, auto_merge_strategy, auto_merge_by, auto_merge_set_at, delete_source_on_merge, merged_by, merged_at, merge_strategy, merged_oid, closed_at, created_at, updated_at, source_findings, attention, origin) FROM stdin;
01a1eee5-cf9c-765c-a36f-360b5b0c1b31	01a1eee5-c4a1-735f-9d08-6e6aa6f67566	3	01a1eee5-c3ed-7195-8e19-b7367c823361	Add a README	Fixes #2.	f	main	01a1eee5-c4a1-735f-9d08-6e6aa6f67566	\\x726566732f68656164732f646f6373	t	merged	\N	\N	\N	\N	t	01a1eee5-c3ed-7195-8e19-b7367c823361	2026-11-18 13:43:21.174222+00	merge-commit	9f96126a44821776bcb75c489267994abef4dc85	\N	2026-11-18 13:43:20.731687+00	2026-11-18 13:43:21.174222+00	\N	ready	writer
01a1eee5-d03e-7509-b00f-1b19eaad6b81	01a1eee5-c4a1-735f-9d08-6e6aa6f67566	4	01a1eee5-c3ed-7195-8e19-b7367c823361	Count to three	Fixes #2.	f	main	01a1eee5-c4a1-735f-9d08-6e6aa6f67566	\\x726566732f68656164732f6669782d61	t	open	\N	\N	\N	\N	t	\N	\N	\N	\N	\N	2026-11-18 13:43:20.892838+00	2026-11-18 13:43:21.434309+00	\N	ready	writer
\.


--
-- Data for Name: milestones; Type: TABLE DATA; Schema: public; Owner: -
--

COPY public.milestones (id, repo_id, title, description, due_on, state, created_at, updated_at, origin) FROM stdin;
01a1eee5-ccde-766c-90b9-58acc3941b22	01a1eee5-c4a1-735f-9d08-6e6aa6f67566	v1.0	\N	2027-01-31	open	2026-11-18 13:43:20.031829+00	2026-11-18 13:43:20.031829+00	writer
\.


--
-- Data for Name: mr_assignees; Type: TABLE DATA; Schema: public; Owner: -
--

COPY public.mr_assignees (mr_id, user_id) FROM stdin;
\.


--
-- Data for Name: mr_change_aliases; Type: TABLE DATA; Schema: public; Owner: -
--

COPY public.mr_change_aliases (mr_id, kind, value, change_id, revision_seq, bound_by, created_at) FROM stdin;
01a1eee5-cf9c-765c-a36f-360b5b0c1b31	jj	kspkvmwlnzzwsuxokuyzwzqnxsqntskw	01a1eee5-cfcf-7563-94f0-b435a0dda5dc	1	01a1eee5-c3ed-7195-8e19-b7367c823361	2026-11-18 13:43:20.779511+00
01a1eee5-d03e-7509-b00f-1b19eaad6b81	jj	vqvxvonnrmzouluypmxkkqvsktsqrlrm	01a1eee5-d06a-778b-b11a-4bbaf059af5e	1	01a1eee5-c3ed-7195-8e19-b7367c823361	2026-11-18 13:43:20.936319+00
\.


--
-- Data for Name: mr_change_versions; Type: TABLE DATA; Schema: public; Owner: -
--

COPY public.mr_change_versions (id, revision_id, change_id, "position", commit_oid, parent_oid, patch_id, state, reason, candidates, conflicted) FROM stdin;
01a1eee5-cfd4-7384-b14c-53829e2a7997	01a1eee5-cfcb-7064-aef6-d6c77d305abf	01a1eee5-cfcf-7563-94f0-b435a0dda5dc	0	d8c56c4dcfe639e4390c08afd4aec0037cb2f5ef	e31a2bdcfa4366d63eec483681f2e7bef2528dd9	6ad69f7b329fade4f3d645423ac425a02b578c53	new	\N	{}	f
01a1eee5-d06d-71c9-95f4-c80066b72463	01a1eee5-d068-7630-8a31-148b8ec397cd	01a1eee5-d06a-778b-b11a-4bbaf059af5e	0	01cca1adb1719e6fe29ff4b58a7ad0b133d24292	e31a2bdcfa4366d63eec483681f2e7bef2528dd9	5f674b0296a9b737bb50b33ea5a1fb66a0d5cf2a	new	\N	{}	f
\.


--
-- Data for Name: mr_changes; Type: TABLE DATA; Schema: public; Owner: -
--

COPY public.mr_changes (id, mr_id, display_id, state, created_at) FROM stdin;
01a1eee5-d06a-778b-b11a-4bbaf059af5e	01a1eee5-d03e-7509-b00f-1b19eaad6b81	vqvxvonnrmzouluypmxkkqvsktsqrlrm	active	2026-11-18 13:43:20.936319+00
01a1eee5-cfcf-7563-94f0-b435a0dda5dc	01a1eee5-cf9c-765c-a36f-360b5b0c1b31	kspkvmwlnzzwsuxokuyzwzqnxsqntskw	merged	2026-11-18 13:43:20.779511+00
\.


--
-- Data for Name: mr_checkouts; Type: TABLE DATA; Schema: public; Owner: -
--

COPY public.mr_checkouts (mr_id, user_id, created_at) FROM stdin;
\.


--
-- Data for Name: mr_labels; Type: TABLE DATA; Schema: public; Owner: -
--

COPY public.mr_labels (mr_id, label_id) FROM stdin;
\.


--
-- Data for Name: mr_ledger; Type: TABLE DATA; Schema: public; Owner: -
--

COPY public.mr_ledger (id, mr_id, revision_seq, kind, change_id, state, reason, detail, actor_id, created_at) FROM stdin;
01a1eee5-cfd6-7151-abf0-738028df56de	01a1eee5-cf9c-765c-a36f-360b5b0c1b31	1	binding	kspkvmwlnzzwsuxokuyzwzqnxsqntskw	new	\N	{"by": null, "commit": "d8c56c4dcfe639e4390c08afd4aec0037cb2f5ef", "candidates": []}	01a1eee5-c3ed-7195-8e19-b7367c823361	2026-11-18 13:43:20.779511+00
01a1eee5-d06f-77a0-861a-de8ac9663109	01a1eee5-d03e-7509-b00f-1b19eaad6b81	1	binding	vqvxvonnrmzouluypmxkkqvsktsqrlrm	new	\N	{"by": null, "commit": "01cca1adb1719e6fe29ff4b58a7ad0b133d24292", "candidates": []}	01a1eee5-c3ed-7195-8e19-b7367c823361	2026-11-18 13:43:20.936319+00
01a1eee5-d182-7325-be5c-6a48a809f025	01a1eee5-d03e-7509-b00f-1b19eaad6b81	\N	target_moved	\N	\N	\N	{"new": "9f96126a44821776bcb75c489267994abef4dc85", "old": "e31a2bdcfa4366d63eec483681f2e7bef2528dd9"}	01a1eee5-c3ed-7195-8e19-b7367c823361	2026-11-18 13:43:21.220133+00
\.


--
-- Data for Name: mr_review_viewed_files; Type: TABLE DATA; Schema: public; Owner: -
--

COPY public.mr_review_viewed_files (review_id, path) FROM stdin;
\.


--
-- Data for Name: mr_reviewers; Type: TABLE DATA; Schema: public; Owner: -
--

COPY public.mr_reviewers (id, mr_id, user_id, team_id, requested_by, state, created_at) FROM stdin;
01a1eee5-d21a-769a-90c6-92a8efccb593	01a1eee5-d03e-7509-b00f-1b19eaad6b81	01a1eee5-c6fa-7690-b280-18dab0b26714	\N	01a1eee5-c3ed-7195-8e19-b7367c823361	reviewed	2026-11-18 13:43:21.37075+00
\.


--
-- Data for Name: mr_reviews; Type: TABLE DATA; Schema: public; Owner: -
--

COPY public.mr_reviews (id, mr_id, revision_seq, reviewer_id, verdict, body, created_at, files_total, origin) FROM stdin;
01a1eee5-d0ea-72b2-8cfb-1c5a21bf19af	01a1eee5-cf9c-765c-a36f-360b5b0c1b31	1	01a1eee5-c6fa-7690-b280-18dab0b26714	approve		2026-11-18 13:43:21.066605+00	1	writer
01a1eee5-d25a-756f-ba0a-ad70ceeb79fc	01a1eee5-d03e-7509-b00f-1b19eaad6b81	1	01a1eee5-c6fa-7690-b280-18dab0b26714	approve	Looks good.	2026-11-18 13:43:21.434309+00	1	writer
\.


--
-- Data for Name: mr_revisions; Type: TABLE DATA; Schema: public; Owner: -
--

COPY public.mr_revisions (id, mr_id, seq, base_oid, merge_base_oid, head_oid, pushed_by, commit_count, mode, mode_reason, missing_objects, created_at, same_patches) FROM stdin;
01a1eee5-cfcb-7064-aef6-d6c77d305abf	01a1eee5-cf9c-765c-a36f-360b5b0c1b31	1	e31a2bdcfa4366d63eec483681f2e7bef2528dd9	e31a2bdcfa4366d63eec483681f2e7bef2528dd9	d8c56c4dcfe639e4390c08afd4aec0037cb2f5ef	01a1eee5-c3ed-7195-8e19-b7367c823361	1	per-change	\N	f	2026-11-18 13:43:20.779511+00	f
01a1eee5-d068-7630-8a31-148b8ec397cd	01a1eee5-d03e-7509-b00f-1b19eaad6b81	1	e31a2bdcfa4366d63eec483681f2e7bef2528dd9	e31a2bdcfa4366d63eec483681f2e7bef2528dd9	01cca1adb1719e6fe29ff4b58a7ad0b133d24292	01a1eee5-c3ed-7195-8e19-b7367c823361	1	per-change	\N	f	2026-11-18 13:43:20.936319+00	f
\.


--
-- Data for Name: mr_thread_comments; Type: TABLE DATA; Schema: public; Owner: -
--

COPY public.mr_thread_comments (id, thread_id, author_id, body, pending, created_at, edited_at, deleted_at, origin, resolves) FROM stdin;
01a1eee5-d1c6-7575-9224-b94e38962602	01a1eee5-d1c4-74ad-8fa9-992e7a2616b0	01a1eee5-c6fa-7690-b280-18dab0b26714	Why three?	f	2026-11-18 13:43:21.284695+00	\N	\N	writer	f
01a1eee5-d1f9-741c-8d50-2a1f9a3aee63	01a1eee5-d1c4-74ad-8fa9-992e7a2616b0	01a1eee5-c3ed-7195-8e19-b7367c823361	The tests count to three.	f	2026-11-18 13:43:21.337132+00	\N	\N	writer	f
\.


--
-- Data for Name: mr_thread_findings; Type: TABLE DATA; Schema: public; Owner: -
--

COPY public.mr_thread_findings (thread_id, mr_id, revision_seq, context, key, severity, category, status_id, resolved_by_status) FROM stdin;
\.


--
-- Data for Name: mr_threads; Type: TABLE DATA; Schema: public; Owner: -
--

COPY public.mr_threads (id, mr_id, revision_seq, old_oid, new_oid, path, side, line_start, line_end, line_hash, context, author_id, resolved_at, resolved_by, created_at, addressed_by, addressed_at) FROM stdin;
01a1eee5-d1c4-74ad-8fa9-992e7a2616b0	01a1eee5-d03e-7509-b00f-1b19eaad6b81	1	e31a2bdcfa4366d63eec483681f2e7bef2528dd9	01cca1adb1719e6fe29ff4b58a7ad0b133d24292	a.txt	new	1	1	4e07408562bedb8b60ce05c1decfe3ad16b72230967de01f640b7e4729b49fce	{"after": [], "lines": ["3"], "before": []}	01a1eee5-c6fa-7690-b280-18dab0b26714	\N	\N	2026-11-18 13:43:21.284695+00	\N	\N
\.


--
-- Data for Name: mr_viewed_files; Type: TABLE DATA; Schema: public; Owner: -
--

COPY public.mr_viewed_files (mr_id, user_id, path, blob_oid) FROM stdin;
\.


--
-- Data for Name: namespaces; Type: TABLE DATA; Schema: public; Owner: -
--

COPY public.namespaces (id, name, kind, created_at) FROM stdin;
01a1eee5-c3ed-7195-8e19-b735f8e3489e	alice	user	2026-11-18 13:43:17.735355+00
01a1eee5-c6fa-7690-b280-18d9bf44af22	bob	user	2026-11-18 13:43:18.518861+00
01a1eee5-c838-7429-8a61-88c68449da23	root-admin	user	2026-11-18 13:43:18.838434+00
01a1eee5-ca23-72a1-a05a-24d90509fc0d	acme	org	2026-11-18 13:43:19.329649+00
01a1eee5-d334-76db-a69c-3c5aec2c1549	acme-ci	user	2026-11-18 13:43:21.652005+00
\.


--
-- Data for Name: notifications; Type: TABLE DATA; Schema: public; Owner: -
--

COPY public.notifications (id, user_id, repo_id, subject_kind, subject_id, reason, unread, updated_at) FROM stdin;
01a1eee5-cd31-7557-9947-625fae53118a	01a1eee5-c6fa-7690-b280-18dab0b26714	01a1eee5-c4a1-735f-9d08-6e6aa6f67566	issue	01a1eee5-ccf7-72f6-95d0-80259c33e486	assigned	f	2026-11-18 13:43:20.184765+00
01a1eee5-ce38-74ab-bb66-c2b6900849b0	01a1eee5-c3ed-7195-8e19-b7367c823361	01a1eee5-c4a1-735f-9d08-6e6aa6f67566	issue	01a1eee5-ccf7-72f6-95d0-80259c33e486	mentioned	t	2026-11-18 13:43:20.377418+00
01a1eee5-d11a-77df-82cc-87e1d2f7241d	01a1eee5-c3ed-7195-8e19-b7367c823361	01a1eee5-c4a1-735f-9d08-6e6aa6f67566	merge_request	01a1eee5-cf9c-765c-a36f-360b5b0c1b31	author	t	2026-11-18 13:43:21.115968+00
01a1eee5-d1ab-76e0-af6c-aae691c62844	01a1eee5-c6fa-7690-b280-18dab0b26714	01a1eee5-c4a1-735f-9d08-6e6aa6f67566	merge_request	01a1eee5-cf9c-765c-a36f-360b5b0c1b31	commented	t	2026-11-18 13:43:21.260328+00
01a1eee5-d220-723a-8411-ee71e527a803	01a1eee5-c6fa-7690-b280-18dab0b26714	01a1eee5-c4a1-735f-9d08-6e6aa6f67566	merge_request	01a1eee5-d03e-7509-b00f-1b19eaad6b81	review_requested	t	2026-11-18 13:43:21.39825+00
01a1eee5-d1ec-73fb-b7c7-996ddaccd2b3	01a1eee5-c3ed-7195-8e19-b7367c823361	01a1eee5-c4a1-735f-9d08-6e6aa6f67566	merge_request	01a1eee5-d03e-7509-b00f-1b19eaad6b81	author	t	2026-11-18 13:43:21.477536+00
\.


--
-- Data for Name: oidc_identities; Type: TABLE DATA; Schema: public; Owner: -
--

COPY public.oidc_identities (issuer, subject, user_id, provider_key, email, created_at, last_login_at) FROM stdin;
\.


--
-- Data for Name: oidc_pending; Type: TABLE DATA; Schema: public; Owner: -
--

COPY public.oidc_pending (token_hash, provider_key, browser_hash, issuer, subject, email, email_verified, preferred_username, display_name, expires_at) FROM stdin;
\.


--
-- Data for Name: oidc_states; Type: TABLE DATA; Schema: public; Owner: -
--

COPY public.oidc_states (state_hash, provider_key, browser_hash, nonce, pkce_verifier, purpose, user_id, next, expires_at) FROM stdin;
\.


--
-- Data for Name: org_members; Type: TABLE DATA; Schema: public; Owner: -
--

COPY public.org_members (org_id, user_id, role, created_at) FROM stdin;
01a1eee5-ca23-72a1-a05a-24dab42879d7	01a1eee5-c3ed-7195-8e19-b7367c823361	owner	2026-11-18 13:43:19.329649+00
01a1eee5-ca23-72a1-a05a-24dab42879d7	01a1eee5-c6fa-7690-b280-18dab0b26714	member	2026-11-18 13:43:19.362749+00
\.


--
-- Data for Name: organizations; Type: TABLE DATA; Schema: public; Owner: -
--

COPY public.organizations (id, namespace_id, display_name, description, default_member_role, created_at, updated_at, require_mfa, avatar_blob_sha256) FROM stdin;
01a1eee5-ca23-72a1-a05a-24dab42879d7	01a1eee5-ca23-72a1-a05a-24d90509fc0d	Acme	\N	none	2026-11-18 13:43:19.329649+00	2026-11-18 13:43:19.329649+00	f	\N
\.


--
-- Data for Name: password_resets; Type: TABLE DATA; Schema: public; Owner: -
--

COPY public.password_resets (token_hash, user_id, expires_at, used_at, created_at) FROM stdin;
\.


--
-- Data for Name: push_events; Type: TABLE DATA; Schema: public; Owner: -
--

COPY public.push_events (id, repo_id, pusher_id, transport, created_at) FROM stdin;
01a1eee5-c9ab-7032-b92a-64611ac32f2d	01a1eee5-c4a1-735f-9d08-6e6aa6f67566	01a1eee5-c3ed-7195-8e19-b7367c823361	http	2026-11-18 13:43:19.211645+00
01a1eee5-cebb-74c1-b320-a3166415c4dc	01a1eee5-c4a1-735f-9d08-6e6aa6f67566	01a1eee5-c3ed-7195-8e19-b7367c823361	http	2026-11-18 13:43:20.507613+00
01a1eee5-cf6a-73db-9693-c74b4ed5892e	01a1eee5-c4a1-735f-9d08-6e6aa6f67566	01a1eee5-c3ed-7195-8e19-b7367c823361	http	2026-11-18 13:43:20.682632+00
01a1eee5-d174-70d4-ae12-c8a8ff9d41e2	01a1eee5-c4a1-735f-9d08-6e6aa6f67566	01a1eee5-c3ed-7195-8e19-b7367c823361	internal	2026-11-18 13:43:21.204565+00
01a1eee5-d184-77d3-9ad5-8bcba0eb6e72	01a1eee5-c4a1-735f-9d08-6e6aa6f67566	01a1eee5-c3ed-7195-8e19-b7367c823361	internal	2026-11-18 13:43:21.220525+00
\.


--
-- Data for Name: recovery_codes; Type: TABLE DATA; Schema: public; Owner: -
--

COPY public.recovery_codes (id, user_id, code_hash, used_at, created_at) FROM stdin;
\.


--
-- Data for Name: ref_updates; Type: TABLE DATA; Schema: public; Owner: -
--

COPY public.ref_updates (push_id, seq, name, old_oid, new_oid) FROM stdin;
01a1eee5-c9ab-7032-b92a-64611ac32f2d	0	\\x726566732f68656164732f6d61696e	\N	e31a2bdcfa4366d63eec483681f2e7bef2528dd9
01a1eee5-c9ab-7032-b92a-64611ac32f2d	1	\\x726566732f746167732f72656c656173652f322e30	\N	822b6c5fab6d985b4dbf8caccf6ca5db811ab916
01a1eee5-c9ab-7032-b92a-64611ac32f2d	2	\\x726566732f746167732f747265652d746167	\N	b952f8c74fe4b4f428492d75c1e227657be2bf48
01a1eee5-c9ab-7032-b92a-64611ac32f2d	3	\\x726566732f746167732f76312e30	\N	032aee9a437815204df8a21bbc7371dcec2bd0e3
01a1eee5-c9ab-7032-b92a-64611ac32f2d	4	\\x726566732f746167732f76312e302d6c69676874	\N	3f3ec41f7a4fefa97602911c0258528cc2ad3d25
01a1eee5-c9ab-7032-b92a-64611ac32f2d	5	\\x726566732f746167732f76322e302d6d657461	\N	e1e0b1fbb57d0584f431150d048088a9e1508dbe
01a1eee5-cebb-74c1-b320-a3166415c4dc	0	\\x726566732f68656164732f646f6373	\N	d8c56c4dcfe639e4390c08afd4aec0037cb2f5ef
01a1eee5-cf6a-73db-9693-c74b4ed5892e	0	\\x726566732f68656164732f6669782d61	\N	01cca1adb1719e6fe29ff4b58a7ad0b133d24292
01a1eee5-d174-70d4-ae12-c8a8ff9d41e2	0	\\x726566732f68656164732f6d61696e	e31a2bdcfa4366d63eec483681f2e7bef2528dd9	9f96126a44821776bcb75c489267994abef4dc85
01a1eee5-d184-77d3-9ad5-8bcba0eb6e72	0	\\x726566732f68656164732f646f6373	d8c56c4dcfe639e4390c08afd4aec0037cb2f5ef	\N
\.


--
-- Data for Name: repo_collaborators; Type: TABLE DATA; Schema: public; Owner: -
--

COPY public.repo_collaborators (repo_id, user_id, role, created_at) FROM stdin;
01a1eee5-ca5d-71b1-a3cc-66b3c21ae6d8	01a1eee5-c3ed-7195-8e19-b7367c823361	admin	2026-11-18 13:43:19.390268+00
01a1eee5-c4a1-735f-9d08-6e6aa6f67566	01a1eee5-c6fa-7690-b280-18dab0b26714	write	2026-11-18 13:43:19.491868+00
\.


--
-- Data for Name: repo_forks; Type: TABLE DATA; Schema: public; Owner: -
--

COPY public.repo_forks (repo_id, parent_id, default_branch_only, max_visibility, state, error, requested_by, created_at, finished_at) FROM stdin;
01a1eee5-d2a8-7404-a045-392ebd0a669f	01a1eee5-ca5d-71b1-a3cc-66b3c21ae6d8	f	public	done	\N	01a1eee5-c6fa-7690-b280-18dab0b26714	2026-11-18 13:43:21.512413+00	2026-11-18 13:43:21.651786+00
\.


--
-- Data for Name: repo_imports; Type: TABLE DATA; Schema: public; Owner: -
--

COPY public.repo_imports (repo_id, url, requested_by, state, error, branches, tags, size_bytes, created_at, started_at, finished_at, secret_findings, secret_findings_allowed) FROM stdin;
\.


--
-- Data for Name: repo_redirects; Type: TABLE DATA; Schema: public; Owner: -
--

COPY public.repo_redirects (namespace_id, name, repo_id, created_at) FROM stdin;
01a1eee5-c3ed-7195-8e19-b735f8e3489e	renamed	01a1eee5-cad5-7185-aeca-8aee6aa64a80	2026-11-18 13:43:19.604613+00
\.


--
-- Data for Name: repo_team_access; Type: TABLE DATA; Schema: public; Owner: -
--

COPY public.repo_team_access (repo_id, team_id, role) FROM stdin;
01a1eee5-ca5d-71b1-a3cc-66b3c21ae6d8	01a1eee5-cc44-7239-81f8-c54b5db22bfa	write
\.


--
-- Data for Name: repositories; Type: TABLE DATA; Schema: public; Owner: -
--

COPY public.repositories (id, namespace_id, name, description, visibility, default_branch, state, fork_parent_id, fork_network_id, allow_forking, trust_bot_runs, auto_approve_external_for, issues_enabled, merge_requests_enabled, size_bytes, next_item_number, merge_strategies, default_merge_strategy, delete_source_branch_default, archived_reason, deleted_at, pushed_at, created_at, updated_at, pushes_since_maintenance, maintained_at, maintenance_runs, maintenance_lease_until, push_protection, secret_allowance_role, sensitive_paths, description_origin) FROM stdin;
01a1eee5-ca5d-71b1-a3cc-66b3c21ae6d8	01a1eee5-ca23-72a1-a05a-24d90509fc0d	tools	\N	public	main	active	\N	01a1eee5-ca5d-71b1-a3cc-66b3c21ae6d8	t	f	none	t	t	0	1	{fast-forward,rebase,merge-commit,squash}	rebase	t	\N	\N	\N	2026-11-18 13:43:19.390268+00	2026-11-18 13:43:19.390268+00	0	\N	0	\N	block	write	{}	writer
01a1eee5-cad5-7185-aeca-8aee6aa64a80	01a1eee5-c3ed-7195-8e19-b735f8e3489e	new-name	\N	private	main	active	\N	01a1eee5-cad5-7185-aeca-8aee6aa64a80	t	f	none	t	t	0	1	{fast-forward,rebase,merge-commit,squash}	rebase	t	\N	\N	\N	2026-11-18 13:43:19.509315+00	2026-11-18 13:43:19.604613+00	0	\N	0	\N	block	write	{}	writer
01a1eee5-cbf3-7345-bbc7-f242f7adf4bb	01a1eee5-c3ed-7195-8e19-b735f8e3489e	inside	\N	internal	main	active	\N	01a1eee5-cbf3-7345-bbc7-f242f7adf4bb	t	f	none	t	t	0	1	{fast-forward,rebase,merge-commit,squash}	rebase	t	\N	\N	\N	2026-11-18 13:43:19.795918+00	2026-11-18 13:43:19.795918+00	0	\N	0	\N	block	write	{}	writer
01a1eee5-d2a8-7404-a045-392ebd0a669f	01a1eee5-c6fa-7690-b280-18d9bf44af22	tools	\N	public	main	active	01a1eee5-ca5d-71b1-a3cc-66b3c21ae6d8	01a1eee5-ca5d-71b1-a3cc-66b3c21ae6d8	t	f	none	t	t	0	1	{fast-forward,rebase,merge-commit,squash}	rebase	t	\N	\N	\N	2026-11-18 13:43:21.512413+00	2026-11-18 13:43:21.512413+00	0	\N	0	\N	block	write	{}	writer
01a1eee5-c4a1-735f-9d08-6e6aa6f67566	01a1eee5-c3ed-7195-8e19-b735f8e3489e	demo	\N	private	main	active	\N	01a1eee5-c4a1-735f-9d08-6e6aa6f67566	t	f	none	t	t	65536	5	{fast-forward,rebase,merge-commit,squash}	rebase	t	\N	\N	2026-11-18 13:43:21.220525+00	2026-11-18 13:43:17.922701+00	2026-11-18 13:43:21.640032+00	5	\N	0	\N	block	write	{deploy/**}	writer
\.


--
-- Data for Name: reserved_names; Type: TABLE DATA; Schema: public; Owner: -
--

COPY public.reserved_names (name) FROM stdin;
-
about
admin
api
assets
explore
favicon.ico
ghost
git
help
login
logout
new
notifications
org
orgs
robots.txt
root
search
settings
signup
slath
static
system
user
users
\.


--
-- Data for Name: schema_info; Type: TABLE DATA; Schema: public; Owner: -
--

COPY public.schema_info (version, singleton) FROM stdin;
139	t
\.


--
-- Data for Name: secret_allowances; Type: TABLE DATA; Schema: public; Owner: -
--

COPY public.secret_allowances (id, repo_id, fingerprint, rule, created_by, reason, created_at, expires_at, description) FROM stdin;
\.


--
-- Data for Name: secret_rejections; Type: TABLE DATA; Schema: public; Owner: -
--

COPY public.secret_rejections (id, repo_id, findings, created_at, expires_at, kind) FROM stdin;
\.


--
-- Data for Name: sessions; Type: TABLE DATA; Schema: public; Owner: -
--

COPY public.sessions (id_hash, user_id, created_at, last_seen_at, expires_at, auth_level, sudo_until, ip, user_agent) FROM stdin;
\\xdcdfbd3d3397a3291a7ced7b0ad42fa56cb11cf6789c864759f85f1af92e1574	01a1eee5-c3ed-7195-8e19-b7367c823361	2026-11-18 13:43:17.893681+00	2026-11-18 13:43:17.893681+00	2026-12-18 13:43:17.892445+00	password	2026-11-18 13:58:17.892445+00	127.0.0.1	ureq/3.5.0
\\x12b01d5693be50892074fc437b4d87b8b8941a157bb33da3219208c5c068439c	01a1eee5-c3ed-7195-8e19-b7367c823361	2026-11-18 13:43:18.161024+00	2026-11-18 13:43:18.161024+00	2026-12-18 13:43:18.158188+00	password	2026-11-18 13:58:18.158188+00	127.0.0.1	ureq/3.5.0
\\x797a7cf863523e1135df11a1dee5a8d023e5adf97e9b1d07ec895cab70ea4ed4	01a1eee5-c3ed-7195-8e19-b7367c823361	2026-11-18 13:43:19.31885+00	2026-11-18 13:43:19.31885+00	2026-12-18 13:43:19.318233+00	password	2026-11-18 13:58:19.318233+00	127.0.0.1	ureq/3.5.0
\\x166d5e0d96ce7a4b88c9972468d1c7d1c8674a2b3ae58aca7ef32f1847b5726f	01a1eee5-c6fa-7690-b280-18dab0b26714	2026-11-18 13:43:19.68904+00	2026-11-18 13:43:19.68904+00	2026-12-18 13:43:19.688328+00	password	2026-11-18 13:58:19.688328+00	127.0.0.1	ureq/3.5.0
\\xa0c26c7f6ce6d0aeede6b606d775c49c7c671360200ee59273e0467c39a81aff	01a1eee5-c6fa-7690-b280-18dab0b26714	2026-11-18 13:43:19.761599+00	2026-11-18 13:43:19.761599+00	2026-12-18 13:43:19.760684+00	password	2026-11-18 13:58:19.760684+00	127.0.0.1	ureq/3.5.0
\.


--
-- Data for Name: ssh_keys; Type: TABLE DATA; Schema: public; Owner: -
--

COPY public.ssh_keys (id, user_id, title, key_type, public_key, fingerprint_sha256, last_used_at, created_at, repo_id, can_push, added_by) FROM stdin;
01a1eee5-cb96-71f1-89e9-6062771b3315	01a1eee5-c6fa-7690-b280-18dab0b26714	test key	ssh-ed25519	AAAAC3NzaC1lZDI1NTE5AAAAIO3XF7i+NCwn3yjNdWCKTm37Jw08duD+mtk4k69eUDy2	SHA256:sMJlVCYio5wL0l3yFFRe77bTryQ0r/XurKJKJ6UZnYE	\N	2026-11-18 13:43:19.703381+00	\N	f	\N
01a1eee5-d316-700c-8c9a-5df9513d562d	\N	deploy	ssh-ed25519	AAAAC3NzaC1lZDI1NTE5AAAAIMLW6gAdyg0ywfAGqXG7brEKF3l2RfXfl3y+x1vSRTJU	SHA256:hcbw8vHb0kmeCAIz4XZxW/xIXPZbOzVB0n1K5zwdFoo	\N	2026-11-18 13:43:21.623437+00	01a1eee5-c4a1-735f-9d08-6e6aa6f67566	f	01a1eee5-c3ed-7195-8e19-b7367c823361
\.


--
-- Data for Name: status_annotations; Type: TABLE DATA; Schema: public; Owner: -
--

COPY public.status_annotations (status_id, ordinal, path, line_start, line_end, severity, category, message) FROM stdin;
\.


--
-- Data for Name: subscriptions; Type: TABLE DATA; Schema: public; Owner: -
--

COPY public.subscriptions (user_id, subject_kind, subject_id, repo_id, state, created_at) FROM stdin;
01a1eee5-c3ed-7195-8e19-b7367c823361	repo	01a1eee5-c4a1-735f-9d08-6e6aa6f67566	01a1eee5-c4a1-735f-9d08-6e6aa6f67566	watching	2026-11-18 13:43:17.922701+00
01a1eee5-c3ed-7195-8e19-b7367c823361	repo	01a1eee5-ca5d-71b1-a3cc-66b3c21ae6d8	01a1eee5-ca5d-71b1-a3cc-66b3c21ae6d8	watching	2026-11-18 13:43:19.390268+00
01a1eee5-c3ed-7195-8e19-b7367c823361	repo	01a1eee5-cad5-7185-aeca-8aee6aa64a80	01a1eee5-cad5-7185-aeca-8aee6aa64a80	watching	2026-11-18 13:43:19.509315+00
01a1eee5-c3ed-7195-8e19-b7367c823361	repo	01a1eee5-cbf3-7345-bbc7-f242f7adf4bb	01a1eee5-cbf3-7345-bbc7-f242f7adf4bb	watching	2026-11-18 13:43:19.795918+00
01a1eee5-c6fa-7690-b280-18dab0b26714	repo	01a1eee5-d2a8-7404-a045-392ebd0a669f	01a1eee5-d2a8-7404-a045-392ebd0a669f	watching	2026-11-18 13:43:21.512413+00
\.


--
-- Data for Name: team_members; Type: TABLE DATA; Schema: public; Owner: -
--

COPY public.team_members (team_id, user_id) FROM stdin;
01a1eee5-cc44-7239-81f8-c54b5db22bfa	01a1eee5-c6fa-7690-b280-18dab0b26714
\.


--
-- Data for Name: teams; Type: TABLE DATA; Schema: public; Owner: -
--

COPY public.teams (id, org_id, name, description, created_at) FROM stdin;
01a1eee5-cc44-7239-81f8-c54b5db22bfa	01a1eee5-ca23-72a1-a05a-24dab42879d7	core	Core team	2026-11-18 13:43:19.877739+00
\.


--
-- Data for Name: timeline_events; Type: TABLE DATA; Schema: public; Owner: -
--

COPY public.timeline_events (id, repo_id, subject_kind, subject_id, actor_id, kind, data, created_at) FROM stdin;
01a1eee5-cd32-743c-b7d1-a2adb09c2f49	01a1eee5-c4a1-735f-9d08-6e6aa6f67566	issue	01a1eee5-ccf7-72f6-95d0-80259c33e486	01a1eee5-c3ed-7195-8e19-b7367c823361	labeled	{"label": "bug"}	2026-11-18 13:43:20.11543+00
01a1eee5-cd38-7716-9242-d1ff089c8ff6	01a1eee5-c4a1-735f-9d08-6e6aa6f67566	issue	01a1eee5-ccf7-72f6-95d0-80259c33e486	01a1eee5-c3ed-7195-8e19-b7367c823361	labeled	{"label": "good first issue"}	2026-11-18 13:43:20.120745+00
01a1eee5-cd51-74cb-8896-7f4b7f850d05	01a1eee5-c4a1-735f-9d08-6e6aa6f67566	issue	01a1eee5-ccf7-72f6-95d0-80259c33e486	01a1eee5-c3ed-7195-8e19-b7367c823361	assigned	{"user": "bob"}	2026-11-18 13:43:20.147475+00
01a1eee5-cd67-729b-a43c-d8b9c1e04df0	01a1eee5-c4a1-735f-9d08-6e6aa6f67566	issue	01a1eee5-ccf7-72f6-95d0-80259c33e486	01a1eee5-c3ed-7195-8e19-b7367c823361	milestoned	{"milestone": "v1.0"}	2026-11-18 13:43:20.168715+00
01a1eee5-cdb8-761c-83d3-544cdd529ba0	01a1eee5-c4a1-735f-9d08-6e6aa6f67566	issue	01a1eee5-cd91-717a-a190-339a2fb7dcef	01a1eee5-c3ed-7195-8e19-b7367c823361	closed	{"reason": "completed"}	2026-11-18 13:43:20.245751+00
01a1eee5-ce12-774e-b4cd-8223e534bd88	01a1eee5-c4a1-735f-9d08-6e6aa6f67566	issue	01a1eee5-cd91-717a-a190-339a2fb7dcef	01a1eee5-c6fa-7690-b280-18dab0b26714	referenced	{"kind": "issue", "number": 1, "repo_id": "01a1eee5-c4a1-735f-9d08-6e6aa6f67566"}	2026-11-18 13:43:20.339042+00
01a1eee5-d007-7195-b553-2d5553ee5cd8	01a1eee5-c4a1-735f-9d08-6e6aa6f67566	issue	01a1eee5-cd91-717a-a190-339a2fb7dcef	01a1eee5-c3ed-7195-8e19-b7367c823361	referenced	{"kind": "merge_request", "number": 3, "repo_id": "01a1eee5-c4a1-735f-9d08-6e6aa6f67566"}	2026-11-18 13:43:20.840382+00
01a1eee5-d0a1-7468-95e7-018af927e0e7	01a1eee5-c4a1-735f-9d08-6e6aa6f67566	issue	01a1eee5-cd91-717a-a190-339a2fb7dcef	01a1eee5-c3ed-7195-8e19-b7367c823361	referenced	{"kind": "merge_request", "number": 4, "repo_id": "01a1eee5-c4a1-735f-9d08-6e6aa6f67566"}	2026-11-18 13:43:20.994092+00
01a1eee5-d172-75dd-aed0-b77f496f69e5	01a1eee5-c4a1-735f-9d08-6e6aa6f67566	merge_request	01a1eee5-cf9c-765c-a36f-360b5b0c1b31	01a1eee5-c3ed-7195-8e19-b7367c823361	merged	{"oid": "9f96126a44821776bcb75c489267994abef4dc85", "target": "main", "strategy": "merge-commit"}	2026-11-18 13:43:21.174222+00
01a1eee5-d21e-76cc-bb00-fb205dea8891	01a1eee5-c4a1-735f-9d08-6e6aa6f67566	merge_request	01a1eee5-d03e-7509-b00f-1b19eaad6b81	01a1eee5-c3ed-7195-8e19-b7367c823361	review_requested	{"user": "bob"}	2026-11-18 13:43:21.37075+00
\.


--
-- Data for Name: totp_credentials; Type: TABLE DATA; Schema: public; Owner: -
--

COPY public.totp_credentials (user_id, secret_enc, key_id, last_used_step, confirmed, created_at) FROM stdin;
\.


--
-- Data for Name: upgrade_history; Type: TABLE DATA; Schema: public; Owner: -
--

COPY public.upgrade_history (id, from_version, to_version, from_schema, to_schema, backup_path, preflight, started_at, finished_at, outcome, error) FROM stdin;
\.


--
-- Data for Name: user_emails; Type: TABLE DATA; Schema: public; Owner: -
--

COPY public.user_emails (user_id, email, verified, created_at) FROM stdin;
\.


--
-- Data for Name: users; Type: TABLE DATA; Schema: public; Owner: -
--

COPY public.users (id, namespace_id, kind, bot_owner_kind, bot_owner_id, display_name, email, email_verified, password_hash, is_admin, state, mfa_required, avatar_blob_sha256, timezone, theme, diff_view, editor_mode, created_at, updated_at, density, code_font_size, email_notifications, diff_ignore_whitespace, vcs_tool, pushed_tool, suspended_with_owner, stopped_at, stopped_by) FROM stdin;
01a1eee5-c6fa-7690-b280-18dab0b26714	01a1eee5-c6fa-7690-b280-18d9bf44af22	human	\N	\N	\N	bob@example.org	f	$argon2id$v=19$m=19456,t=2,p=1$SvAqjPfAo895sHi6jlTv8g$6eb4yTn7xTYYZzKxrLhu71rfmPC8+pnmB7lMKFRDj4I	f	active	f	\N	\N	system	unified	wysiwyg	2026-11-18 13:43:18.518861+00	2026-11-18 13:43:18.518861+00	comfortable	14	t	f	\N	\N	f	\N	\N
01a1eee5-c838-7429-8a61-88c71aeb2ef0	01a1eee5-c838-7429-8a61-88c68449da23	human	\N	\N	\N	root-admin@example.org	f	$argon2id$v=19$m=19456,t=2,p=1$c6zaZ0OY3na5lPKFbjGwlw$MUcyjU6BAxDlouWBJMJxm3l1z8wSIKpkhFrq2HVUa3E	t	active	f	\N	\N	system	unified	wysiwyg	2026-11-18 13:43:18.838434+00	2026-11-18 13:43:18.838434+00	comfortable	14	t	f	\N	\N	f	\N	\N
01a1eee5-c3ed-7195-8e19-b7367c823361	01a1eee5-c3ed-7195-8e19-b735f8e3489e	human	\N	\N	\N	alice@example.org	f	$argon2id$v=19$m=19456,t=2,p=1$q1D88PDNGwpHjtOW8GiIdw$ZEvIGgsjewmrk1o0hq/NtAY/OCQdnvaa81uIm1NZrvU	f	active	f	\N	\N	system	unified	wysiwyg	2026-11-18 13:43:17.735355+00	2026-11-18 13:43:17.735355+00	comfortable	14	t	f	\N	git	f	\N	\N
01a1eee5-d334-76db-a69c-3c5bfe800e10	01a1eee5-d334-76db-a69c-3c5aec2c1549	bot	org	01a1eee5-ca23-72a1-a05a-24dab42879d7	Acme CI	\N	f	\N	f	active	f	\N	\N	system	unified	wysiwyg	2026-11-18 13:43:21.652005+00	2026-11-18 13:43:21.652005+00	comfortable	14	t	f	\N	\N	f	\N	\N
\.


--
-- Data for Name: webauthn_ceremonies; Type: TABLE DATA; Schema: public; Owner: -
--

COPY public.webauthn_ceremonies (id_hash, user_id, kind, state, expires_at, created_at) FROM stdin;
\.


--
-- Data for Name: webauthn_credentials; Type: TABLE DATA; Schema: public; Owner: -
--

COPY public.webauthn_credentials (id, user_id, credential_id, passkey, name, last_used_at, created_at) FROM stdin;
\.


--
-- Data for Name: webhook_attempts; Type: TABLE DATA; Schema: public; Owner: -
--

COPY public.webhook_attempts (delivery_id, attempt, started_at, duration_ms, status, response, error) FROM stdin;
\.


--
-- Data for Name: webhook_deliveries; Type: TABLE DATA; Schema: public; Owner: -
--

COPY public.webhook_deliveries (id, webhook_id, event, action, payload, state, attempts, next_attempt_at, redelivery_of, created_at, finished_at) FROM stdin;
\.


--
-- Data for Name: webhooks; Type: TABLE DATA; Schema: public; Owner: -
--

COPY public.webhooks (id, repo_id, org_id, url, secret_enc, key_id, events, active, disabled_reason, consecutive_failures, principal_id, created_at, updated_at) FROM stdin;
01a1eee5-d2fd-72e5-8762-b811e3ce3c7b	01a1eee5-c4a1-735f-9d08-6e6aa6f67566	\N	http://127.0.0.1:9/hook	\\x01618e062d0d8389571bf52e88062dd3b1debf68f1abb069add39ad105022ff7448c31450f0b744e636cc0e4f908a3ccf15cda26ec86d8fe5e41c4	7d3b01b9	{push,merge_request}	t	\N	0	01a1eee5-c3ed-7195-8e19-b7367c823361	2026-11-18 13:43:21.601587+00	2026-11-18 13:43:21.601587+00
\.


--
-- Name: jobs_id_seq; Type: SEQUENCE SET; Schema: public; Owner: -
--

SELECT pg_catalog.setval('public.jobs_id_seq', 21, true);


--
-- Name: _sqlx_migrations _sqlx_migrations_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public._sqlx_migrations
    ADD CONSTRAINT _sqlx_migrations_pkey PRIMARY KEY (version);


--
-- Name: access_tokens access_tokens_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.access_tokens
    ADD CONSTRAINT access_tokens_pkey PRIMARY KEY (id);


--
-- Name: access_tokens access_tokens_token_hash_key; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.access_tokens
    ADD CONSTRAINT access_tokens_token_hash_key UNIQUE (token_hash);


--
-- Name: attachments attachments_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.attachments
    ADD CONSTRAINT attachments_pkey PRIMARY KEY (id);


--
-- Name: audit_events audit_events_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.audit_events
    ADD CONSTRAINT audit_events_pkey PRIMARY KEY (id);


--
-- Name: background_migrations background_migrations_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.background_migrations
    ADD CONSTRAINT background_migrations_pkey PRIMARY KEY (name);


--
-- Name: blobs blobs_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.blobs
    ADD CONSTRAINT blobs_pkey PRIMARY KEY (sha256);


--
-- Name: bot_commits bot_commits_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.bot_commits
    ADD CONSTRAINT bot_commits_pkey PRIMARY KEY (network_id, oid);


--
-- Name: bot_freezes bot_freezes_org_id_key; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.bot_freezes
    ADD CONSTRAINT bot_freezes_org_id_key UNIQUE NULLS NOT DISTINCT (org_id);


--
-- Name: branch_protections branch_protections_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.branch_protections
    ADD CONSTRAINT branch_protections_pkey PRIMARY KEY (id);


--
-- Name: branch_protections branch_protections_repo_id_pattern_key; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.branch_protections
    ADD CONSTRAINT branch_protections_repo_id_pattern_key UNIQUE (repo_id, pattern);


--
-- Name: comments comments_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.comments
    ADD CONSTRAINT comments_pkey PRIMARY KEY (id);


--
-- Name: commit_statuses commit_statuses_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.commit_statuses
    ADD CONSTRAINT commit_statuses_pkey PRIMARY KEY (id);


--
-- Name: email_verifications email_verifications_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.email_verifications
    ADD CONSTRAINT email_verifications_pkey PRIMARY KEY (token_hash);


--
-- Name: encryption_keys encryption_keys_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.encryption_keys
    ADD CONSTRAINT encryption_keys_pkey PRIMARY KEY (key_id);


--
-- Name: git_refs git_refs_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.git_refs
    ADD CONSTRAINT git_refs_pkey PRIMARY KEY (repo_id, name);


--
-- Name: idempotency_keys idempotency_keys_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.idempotency_keys
    ADD CONSTRAINT idempotency_keys_pkey PRIMARY KEY (principal_id, key);


--
-- Name: instance_logos instance_logos_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.instance_logos
    ADD CONSTRAINT instance_logos_pkey PRIMARY KEY (theme);


--
-- Name: instance_settings instance_settings_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.instance_settings
    ADD CONSTRAINT instance_settings_pkey PRIMARY KEY (key);


--
-- Name: invitations invitations_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.invitations
    ADD CONSTRAINT invitations_pkey PRIMARY KEY (id);


--
-- Name: invitations invitations_token_hash_key; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.invitations
    ADD CONSTRAINT invitations_token_hash_key UNIQUE (token_hash);


--
-- Name: issue_assignees issue_assignees_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.issue_assignees
    ADD CONSTRAINT issue_assignees_pkey PRIMARY KEY (issue_id, user_id);


--
-- Name: issue_labels issue_labels_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.issue_labels
    ADD CONSTRAINT issue_labels_pkey PRIMARY KEY (issue_id, label_id);


--
-- Name: issues issues_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.issues
    ADD CONSTRAINT issues_pkey PRIMARY KEY (id);


--
-- Name: issues issues_repo_id_number_key; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.issues
    ADD CONSTRAINT issues_repo_id_number_key UNIQUE (repo_id, number);


--
-- Name: jobs jobs_dedupe_key_key; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.jobs
    ADD CONSTRAINT jobs_dedupe_key_key UNIQUE (dedupe_key);


--
-- Name: jobs jobs_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.jobs
    ADD CONSTRAINT jobs_pkey PRIMARY KEY (id);


--
-- Name: labels labels_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.labels
    ADD CONSTRAINT labels_pkey PRIMARY KEY (id);


--
-- Name: labels labels_repo_id_name_key; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.labels
    ADD CONSTRAINT labels_repo_id_name_key UNIQUE (repo_id, name);


--
-- Name: login_attempts login_attempts_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.login_attempts
    ADD CONSTRAINT login_attempts_pkey PRIMARY KEY (key, window_start);


--
-- Name: login_challenges login_challenges_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.login_challenges
    ADD CONSTRAINT login_challenges_pkey PRIMARY KEY (id_hash);


--
-- Name: merge_requests merge_requests_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.merge_requests
    ADD CONSTRAINT merge_requests_pkey PRIMARY KEY (id);


--
-- Name: merge_requests merge_requests_repo_id_number_key; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.merge_requests
    ADD CONSTRAINT merge_requests_repo_id_number_key UNIQUE (repo_id, number);


--
-- Name: milestones milestones_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.milestones
    ADD CONSTRAINT milestones_pkey PRIMARY KEY (id);


--
-- Name: milestones milestones_repo_id_title_key; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.milestones
    ADD CONSTRAINT milestones_repo_id_title_key UNIQUE (repo_id, title);


--
-- Name: mr_assignees mr_assignees_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.mr_assignees
    ADD CONSTRAINT mr_assignees_pkey PRIMARY KEY (mr_id, user_id);


--
-- Name: mr_change_aliases mr_change_aliases_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.mr_change_aliases
    ADD CONSTRAINT mr_change_aliases_pkey PRIMARY KEY (mr_id, kind, value);


--
-- Name: mr_change_versions mr_change_versions_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.mr_change_versions
    ADD CONSTRAINT mr_change_versions_pkey PRIMARY KEY (id);


--
-- Name: mr_change_versions mr_change_versions_revision_id_position_key; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.mr_change_versions
    ADD CONSTRAINT mr_change_versions_revision_id_position_key UNIQUE (revision_id, "position");


--
-- Name: mr_changes mr_changes_mr_id_display_id_key; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.mr_changes
    ADD CONSTRAINT mr_changes_mr_id_display_id_key UNIQUE (mr_id, display_id);


--
-- Name: mr_changes mr_changes_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.mr_changes
    ADD CONSTRAINT mr_changes_pkey PRIMARY KEY (id);


--
-- Name: mr_checkouts mr_checkouts_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.mr_checkouts
    ADD CONSTRAINT mr_checkouts_pkey PRIMARY KEY (mr_id, user_id);


--
-- Name: mr_labels mr_labels_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.mr_labels
    ADD CONSTRAINT mr_labels_pkey PRIMARY KEY (mr_id, label_id);


--
-- Name: mr_ledger mr_ledger_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.mr_ledger
    ADD CONSTRAINT mr_ledger_pkey PRIMARY KEY (id);


--
-- Name: mr_review_viewed_files mr_review_viewed_files_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.mr_review_viewed_files
    ADD CONSTRAINT mr_review_viewed_files_pkey PRIMARY KEY (review_id, path);


--
-- Name: mr_reviewers mr_reviewers_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.mr_reviewers
    ADD CONSTRAINT mr_reviewers_pkey PRIMARY KEY (id);


--
-- Name: mr_reviews mr_reviews_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.mr_reviews
    ADD CONSTRAINT mr_reviews_pkey PRIMARY KEY (id);


--
-- Name: mr_revisions mr_revisions_mr_id_seq_key; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.mr_revisions
    ADD CONSTRAINT mr_revisions_mr_id_seq_key UNIQUE (mr_id, seq);


--
-- Name: mr_revisions mr_revisions_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.mr_revisions
    ADD CONSTRAINT mr_revisions_pkey PRIMARY KEY (id);


--
-- Name: mr_thread_comments mr_thread_comments_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.mr_thread_comments
    ADD CONSTRAINT mr_thread_comments_pkey PRIMARY KEY (id);


--
-- Name: mr_thread_findings mr_thread_findings_mr_id_revision_seq_context_key_key; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.mr_thread_findings
    ADD CONSTRAINT mr_thread_findings_mr_id_revision_seq_context_key_key UNIQUE (mr_id, revision_seq, context, key);


--
-- Name: mr_thread_findings mr_thread_findings_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.mr_thread_findings
    ADD CONSTRAINT mr_thread_findings_pkey PRIMARY KEY (thread_id);


--
-- Name: mr_threads mr_threads_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.mr_threads
    ADD CONSTRAINT mr_threads_pkey PRIMARY KEY (id);


--
-- Name: mr_viewed_files mr_viewed_files_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.mr_viewed_files
    ADD CONSTRAINT mr_viewed_files_pkey PRIMARY KEY (mr_id, user_id, path);


--
-- Name: namespaces namespaces_name_key; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.namespaces
    ADD CONSTRAINT namespaces_name_key UNIQUE (name);


--
-- Name: namespaces namespaces_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.namespaces
    ADD CONSTRAINT namespaces_pkey PRIMARY KEY (id);


--
-- Name: notifications notifications_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.notifications
    ADD CONSTRAINT notifications_pkey PRIMARY KEY (id);


--
-- Name: notifications notifications_user_id_subject_kind_subject_id_key; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.notifications
    ADD CONSTRAINT notifications_user_id_subject_kind_subject_id_key UNIQUE (user_id, subject_kind, subject_id);


--
-- Name: oidc_identities oidc_identities_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.oidc_identities
    ADD CONSTRAINT oidc_identities_pkey PRIMARY KEY (issuer, subject);


--
-- Name: oidc_pending oidc_pending_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.oidc_pending
    ADD CONSTRAINT oidc_pending_pkey PRIMARY KEY (token_hash);


--
-- Name: oidc_states oidc_states_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.oidc_states
    ADD CONSTRAINT oidc_states_pkey PRIMARY KEY (state_hash);


--
-- Name: org_members org_members_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.org_members
    ADD CONSTRAINT org_members_pkey PRIMARY KEY (org_id, user_id);


--
-- Name: organizations organizations_namespace_id_key; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.organizations
    ADD CONSTRAINT organizations_namespace_id_key UNIQUE (namespace_id);


--
-- Name: organizations organizations_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.organizations
    ADD CONSTRAINT organizations_pkey PRIMARY KEY (id);


--
-- Name: password_resets password_resets_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.password_resets
    ADD CONSTRAINT password_resets_pkey PRIMARY KEY (token_hash);


--
-- Name: push_events push_events_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.push_events
    ADD CONSTRAINT push_events_pkey PRIMARY KEY (id);


--
-- Name: recovery_codes recovery_codes_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.recovery_codes
    ADD CONSTRAINT recovery_codes_pkey PRIMARY KEY (id);


--
-- Name: ref_updates ref_updates_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.ref_updates
    ADD CONSTRAINT ref_updates_pkey PRIMARY KEY (push_id, seq);


--
-- Name: repo_collaborators repo_collaborators_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.repo_collaborators
    ADD CONSTRAINT repo_collaborators_pkey PRIMARY KEY (repo_id, user_id);


--
-- Name: repo_forks repo_forks_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.repo_forks
    ADD CONSTRAINT repo_forks_pkey PRIMARY KEY (repo_id);


--
-- Name: repo_imports repo_imports_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.repo_imports
    ADD CONSTRAINT repo_imports_pkey PRIMARY KEY (repo_id);


--
-- Name: repo_redirects repo_redirects_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.repo_redirects
    ADD CONSTRAINT repo_redirects_pkey PRIMARY KEY (namespace_id, name);


--
-- Name: repo_team_access repo_team_access_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.repo_team_access
    ADD CONSTRAINT repo_team_access_pkey PRIMARY KEY (repo_id, team_id);


--
-- Name: repositories repositories_namespace_id_name_key; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.repositories
    ADD CONSTRAINT repositories_namespace_id_name_key UNIQUE (namespace_id, name);


--
-- Name: repositories repositories_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.repositories
    ADD CONSTRAINT repositories_pkey PRIMARY KEY (id);


--
-- Name: reserved_names reserved_names_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.reserved_names
    ADD CONSTRAINT reserved_names_pkey PRIMARY KEY (name);


--
-- Name: schema_info schema_info_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.schema_info
    ADD CONSTRAINT schema_info_pkey PRIMARY KEY (singleton);


--
-- Name: secret_allowances secret_allowances_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.secret_allowances
    ADD CONSTRAINT secret_allowances_pkey PRIMARY KEY (id);


--
-- Name: secret_rejections secret_rejections_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.secret_rejections
    ADD CONSTRAINT secret_rejections_pkey PRIMARY KEY (id);


--
-- Name: sessions sessions_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.sessions
    ADD CONSTRAINT sessions_pkey PRIMARY KEY (id_hash);


--
-- Name: ssh_keys ssh_keys_fingerprint_sha256_key; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.ssh_keys
    ADD CONSTRAINT ssh_keys_fingerprint_sha256_key UNIQUE (fingerprint_sha256);


--
-- Name: ssh_keys ssh_keys_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.ssh_keys
    ADD CONSTRAINT ssh_keys_pkey PRIMARY KEY (id);


--
-- Name: status_annotations status_annotations_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.status_annotations
    ADD CONSTRAINT status_annotations_pkey PRIMARY KEY (status_id, ordinal);


--
-- Name: subscriptions subscriptions_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.subscriptions
    ADD CONSTRAINT subscriptions_pkey PRIMARY KEY (user_id, subject_kind, subject_id);


--
-- Name: team_members team_members_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.team_members
    ADD CONSTRAINT team_members_pkey PRIMARY KEY (team_id, user_id);


--
-- Name: teams teams_org_id_name_key; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.teams
    ADD CONSTRAINT teams_org_id_name_key UNIQUE (org_id, name);


--
-- Name: teams teams_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.teams
    ADD CONSTRAINT teams_pkey PRIMARY KEY (id);


--
-- Name: timeline_events timeline_events_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.timeline_events
    ADD CONSTRAINT timeline_events_pkey PRIMARY KEY (id);


--
-- Name: totp_credentials totp_credentials_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.totp_credentials
    ADD CONSTRAINT totp_credentials_pkey PRIMARY KEY (user_id);


--
-- Name: upgrade_history upgrade_history_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.upgrade_history
    ADD CONSTRAINT upgrade_history_pkey PRIMARY KEY (id);


--
-- Name: user_emails user_emails_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.user_emails
    ADD CONSTRAINT user_emails_pkey PRIMARY KEY (user_id, email);


--
-- Name: users users_namespace_id_key; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.users
    ADD CONSTRAINT users_namespace_id_key UNIQUE (namespace_id);


--
-- Name: users users_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.users
    ADD CONSTRAINT users_pkey PRIMARY KEY (id);


--
-- Name: webauthn_ceremonies webauthn_ceremonies_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.webauthn_ceremonies
    ADD CONSTRAINT webauthn_ceremonies_pkey PRIMARY KEY (id_hash);


--
-- Name: webauthn_credentials webauthn_credentials_credential_id_key; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.webauthn_credentials
    ADD CONSTRAINT webauthn_credentials_credential_id_key UNIQUE (credential_id);


--
-- Name: webauthn_credentials webauthn_credentials_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.webauthn_credentials
    ADD CONSTRAINT webauthn_credentials_pkey PRIMARY KEY (id);


--
-- Name: webhook_attempts webhook_attempts_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.webhook_attempts
    ADD CONSTRAINT webhook_attempts_pkey PRIMARY KEY (delivery_id, attempt);


--
-- Name: webhook_deliveries webhook_deliveries_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.webhook_deliveries
    ADD CONSTRAINT webhook_deliveries_pkey PRIMARY KEY (id);


--
-- Name: webhooks webhooks_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.webhooks
    ADD CONSTRAINT webhooks_pkey PRIMARY KEY (id);


--
-- Name: access_tokens_user_id_idx; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX access_tokens_user_id_idx ON public.access_tokens USING btree (user_id);


--
-- Name: attachments_blob; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX attachments_blob ON public.attachments USING btree (blob_sha256);


--
-- Name: attachments_repo; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX attachments_repo ON public.attachments USING btree (repo_id, created_at);


--
-- Name: audit_events_created_at_idx; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX audit_events_created_at_idx ON public.audit_events USING btree (created_at);


--
-- Name: blobs_unreferenced; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX blobs_unreferenced ON public.blobs USING btree (unreferenced_since) WHERE (refcount = 0);


--
-- Name: comments_search; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX comments_search ON public.comments USING gin (search);


--
-- Name: comments_subject; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX comments_subject ON public.comments USING btree (subject_kind, subject_id, created_at);


--
-- Name: commit_statuses_lookup; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX commit_statuses_lookup ON public.commit_statuses USING btree (repo_id, commit_oid, context, created_at DESC);


--
-- Name: email_verifications_user; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX email_verifications_user ON public.email_verifications USING btree (user_id);


--
-- Name: idempotency_keys_created_at_idx; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idempotency_keys_created_at_idx ON public.idempotency_keys USING btree (created_at);


--
-- Name: issue_assignees_user; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX issue_assignees_user ON public.issue_assignees USING btree (user_id);


--
-- Name: issue_labels_label; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX issue_labels_label ON public.issue_labels USING btree (label_id);


--
-- Name: issues_author; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX issues_author ON public.issues USING btree (author_id);


--
-- Name: issues_repo_state; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX issues_repo_state ON public.issues USING btree (repo_id, state, updated_at DESC);


--
-- Name: issues_search; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX issues_search ON public.issues USING gin (search);


--
-- Name: jobs_queue_state_run_at_idx; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX jobs_queue_state_run_at_idx ON public.jobs USING btree (queue, state, run_at);


--
-- Name: jobs_running_locked_until_idx; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX jobs_running_locked_until_idx ON public.jobs USING btree (locked_until) WHERE (state = 'running'::text);


--
-- Name: login_attempts_window_start_idx; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX login_attempts_window_start_idx ON public.login_attempts USING btree (window_start);


--
-- Name: login_challenges_expiry; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX login_challenges_expiry ON public.login_challenges USING btree (expires_at);


--
-- Name: merge_requests_author; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX merge_requests_author ON public.merge_requests USING btree (author_id);


--
-- Name: merge_requests_auto_merge; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX merge_requests_auto_merge ON public.merge_requests USING btree (repo_id) WHERE (auto_merge_strategy IS NOT NULL);


--
-- Name: merge_requests_open_pair; Type: INDEX; Schema: public; Owner: -
--

CREATE UNIQUE INDEX merge_requests_open_pair ON public.merge_requests USING btree (source_repo_id, source_ref, repo_id, target_branch) WHERE (state = 'open'::text);


--
-- Name: merge_requests_repo_state; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX merge_requests_repo_state ON public.merge_requests USING btree (repo_id, state, updated_at DESC);


--
-- Name: merge_requests_search; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX merge_requests_search ON public.merge_requests USING gin (search);


--
-- Name: merge_requests_source; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX merge_requests_source ON public.merge_requests USING btree (source_repo_id, source_ref) WHERE (state = 'open'::text);


--
-- Name: merge_requests_waiting_for_checks; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX merge_requests_waiting_for_checks ON public.merge_requests USING btree (repo_id) WHERE ((attention = 'waiting_for_checks'::text) AND (state = 'open'::text));


--
-- Name: mr_assignees_user; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX mr_assignees_user ON public.mr_assignees USING btree (user_id);


--
-- Name: mr_change_aliases_change; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX mr_change_aliases_change ON public.mr_change_aliases USING btree (change_id);


--
-- Name: mr_change_versions_change; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX mr_change_versions_change ON public.mr_change_versions USING btree (change_id);


--
-- Name: mr_changes_mr; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX mr_changes_mr ON public.mr_changes USING btree (mr_id);


--
-- Name: mr_checkouts_user; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX mr_checkouts_user ON public.mr_checkouts USING btree (user_id);


--
-- Name: mr_labels_label; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX mr_labels_label ON public.mr_labels USING btree (label_id);


--
-- Name: mr_ledger_mr; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX mr_ledger_mr ON public.mr_ledger USING btree (mr_id, created_at, id);


--
-- Name: mr_reviewers_requested; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX mr_reviewers_requested ON public.mr_reviewers USING btree (user_id) WHERE (state = 'requested'::text);


--
-- Name: mr_reviewers_team; Type: INDEX; Schema: public; Owner: -
--

CREATE UNIQUE INDEX mr_reviewers_team ON public.mr_reviewers USING btree (mr_id, team_id) WHERE (team_id IS NOT NULL);


--
-- Name: mr_reviewers_user; Type: INDEX; Schema: public; Owner: -
--

CREATE UNIQUE INDEX mr_reviewers_user ON public.mr_reviewers USING btree (mr_id, user_id) WHERE (user_id IS NOT NULL);


--
-- Name: mr_reviews_mr; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX mr_reviews_mr ON public.mr_reviews USING btree (mr_id, created_at);


--
-- Name: mr_reviews_reviewer; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX mr_reviews_reviewer ON public.mr_reviews USING btree (reviewer_id);


--
-- Name: mr_revisions_mr; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX mr_revisions_mr ON public.mr_revisions USING btree (mr_id, seq DESC);


--
-- Name: mr_thread_comments_pending; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX mr_thread_comments_pending ON public.mr_thread_comments USING btree (author_id) WHERE pending;


--
-- Name: mr_thread_comments_thread; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX mr_thread_comments_thread ON public.mr_thread_comments USING btree (thread_id, created_at);


--
-- Name: mr_threads_mr; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX mr_threads_mr ON public.mr_threads USING btree (mr_id, created_at);


--
-- Name: notifications_inbox; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX notifications_inbox ON public.notifications USING btree (user_id, updated_at DESC);


--
-- Name: notifications_unread; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX notifications_unread ON public.notifications USING btree (user_id) WHERE unread;


--
-- Name: oidc_identities_user; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX oidc_identities_user ON public.oidc_identities USING btree (user_id);


--
-- Name: org_members_user_id_idx; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX org_members_user_id_idx ON public.org_members USING btree (user_id);


--
-- Name: password_resets_user; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX password_resets_user ON public.password_resets USING btree (user_id);


--
-- Name: push_events_repo_id_idx; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX push_events_repo_id_idx ON public.push_events USING btree (repo_id, created_at DESC);


--
-- Name: recovery_codes_user; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX recovery_codes_user ON public.recovery_codes USING btree (user_id) WHERE (used_at IS NULL);


--
-- Name: repo_collaborators_user_id_idx; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX repo_collaborators_user_id_idx ON public.repo_collaborators USING btree (user_id);


--
-- Name: repo_forks_parent; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX repo_forks_parent ON public.repo_forks USING btree (parent_id);


--
-- Name: repo_imports_unfinished; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX repo_imports_unfinished ON public.repo_imports USING btree (state) WHERE (state = ANY (ARRAY['queued'::text, 'running'::text]));


--
-- Name: repo_team_access_team; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX repo_team_access_team ON public.repo_team_access USING btree (team_id);


--
-- Name: repositories_description_trgm; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX repositories_description_trgm ON public.repositories USING gin (description public.gin_trgm_ops);


--
-- Name: repositories_fork_network_id_idx; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX repositories_fork_network_id_idx ON public.repositories USING btree (fork_network_id);


--
-- Name: repositories_fork_parent_id_idx; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX repositories_fork_parent_id_idx ON public.repositories USING btree (fork_parent_id);


--
-- Name: repositories_maintenance_due_idx; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX repositories_maintenance_due_idx ON public.repositories USING btree (maintained_at) WHERE ((pushes_since_maintenance > 0) AND (state <> 'deleting'::text));


--
-- Name: repositories_name_trgm; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX repositories_name_trgm ON public.repositories USING gin (((name)::text) public.gin_trgm_ops);


--
-- Name: repositories_public_idx; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX repositories_public_idx ON public.repositories USING btree (updated_at DESC) WHERE ((visibility = 'public'::text) AND (state <> 'deleting'::text));


--
-- Name: secret_allowances_active; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX secret_allowances_active ON public.secret_allowances USING btree (repo_id, fingerprint, expires_at);


--
-- Name: secret_rejections_expiry; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX secret_rejections_expiry ON public.secret_rejections USING btree (expires_at);


--
-- Name: sessions_expires_at_idx; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX sessions_expires_at_idx ON public.sessions USING btree (expires_at);


--
-- Name: sessions_user_id_idx; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX sessions_user_id_idx ON public.sessions USING btree (user_id);


--
-- Name: ssh_keys_repo_id_idx; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX ssh_keys_repo_id_idx ON public.ssh_keys USING btree (repo_id) WHERE (repo_id IS NOT NULL);


--
-- Name: ssh_keys_user_id_idx; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX ssh_keys_user_id_idx ON public.ssh_keys USING btree (user_id);


--
-- Name: subscriptions_subject; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX subscriptions_subject ON public.subscriptions USING btree (subject_kind, subject_id, state);


--
-- Name: team_members_user; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX team_members_user ON public.team_members USING btree (user_id);


--
-- Name: timeline_events_subject; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX timeline_events_subject ON public.timeline_events USING btree (subject_kind, subject_id, created_at);


--
-- Name: totp_credentials_key_id; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX totp_credentials_key_id ON public.totp_credentials USING btree (key_id);


--
-- Name: user_emails_verified_key; Type: INDEX; Schema: public; Owner: -
--

CREATE UNIQUE INDEX user_emails_verified_key ON public.user_emails USING btree (email) WHERE verified;


--
-- Name: users_verified_email_key; Type: INDEX; Schema: public; Owner: -
--

CREATE UNIQUE INDEX users_verified_email_key ON public.users USING btree (email) WHERE email_verified;


--
-- Name: webauthn_ceremonies_expiry; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX webauthn_ceremonies_expiry ON public.webauthn_ceremonies USING btree (expires_at);


--
-- Name: webauthn_credentials_user; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX webauthn_credentials_user ON public.webauthn_credentials USING btree (user_id);


--
-- Name: webhook_deliveries_created; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX webhook_deliveries_created ON public.webhook_deliveries USING btree (created_at);


--
-- Name: webhook_deliveries_hook; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX webhook_deliveries_hook ON public.webhook_deliveries USING btree (webhook_id, created_at DESC);


--
-- Name: webhooks_key_id; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX webhooks_key_id ON public.webhooks USING btree (key_id);


--
-- Name: webhooks_org; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX webhooks_org ON public.webhooks USING btree (org_id) WHERE (org_id IS NOT NULL);


--
-- Name: webhooks_repo; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX webhooks_repo ON public.webhooks USING btree (repo_id) WHERE (repo_id IS NOT NULL);


--
-- Name: attachments attachments_blob_ref; Type: TRIGGER; Schema: public; Owner: -
--

CREATE TRIGGER attachments_blob_ref AFTER INSERT OR DELETE ON public.attachments FOR EACH ROW EXECUTE FUNCTION public.slath_attachment_blob_ref();


--
-- Name: audit_events audit_events_append_only; Type: TRIGGER; Schema: public; Owner: -
--

CREATE TRIGGER audit_events_append_only BEFORE DELETE OR UPDATE ON public.audit_events FOR EACH ROW EXECUTE FUNCTION public.audit_events_append_only();


--
-- Name: instance_logos instance_logos_blob_ref; Type: TRIGGER; Schema: public; Owner: -
--

CREATE TRIGGER instance_logos_blob_ref AFTER INSERT OR DELETE OR UPDATE OF blob_sha256 ON public.instance_logos FOR EACH ROW EXECUTE FUNCTION public.slath_logo_blob_ref();


--
-- Name: organizations organizations_avatar_blob_ref; Type: TRIGGER; Schema: public; Owner: -
--

CREATE TRIGGER organizations_avatar_blob_ref AFTER INSERT OR DELETE OR UPDATE OF avatar_blob_sha256 ON public.organizations FOR EACH ROW EXECUTE FUNCTION public.slath_avatar_blob_ref();


--
-- Name: users users_avatar_blob_ref; Type: TRIGGER; Schema: public; Owner: -
--

CREATE TRIGGER users_avatar_blob_ref AFTER INSERT OR DELETE OR UPDATE OF avatar_blob_sha256 ON public.users FOR EACH ROW EXECUTE FUNCTION public.slath_avatar_blob_ref();


--
-- Name: access_tokens access_tokens_user_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.access_tokens
    ADD CONSTRAINT access_tokens_user_id_fkey FOREIGN KEY (user_id) REFERENCES public.users(id) ON DELETE CASCADE;


--
-- Name: attachments attachments_blob_sha256_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.attachments
    ADD CONSTRAINT attachments_blob_sha256_fkey FOREIGN KEY (blob_sha256) REFERENCES public.blobs(sha256);


--
-- Name: attachments attachments_repo_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.attachments
    ADD CONSTRAINT attachments_repo_id_fkey FOREIGN KEY (repo_id) REFERENCES public.repositories(id) ON DELETE CASCADE;


--
-- Name: attachments attachments_uploader_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.attachments
    ADD CONSTRAINT attachments_uploader_id_fkey FOREIGN KEY (uploader_id) REFERENCES public.users(id) ON DELETE SET NULL;


--
-- Name: bot_commits bot_commits_bot_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.bot_commits
    ADD CONSTRAINT bot_commits_bot_id_fkey FOREIGN KEY (bot_id) REFERENCES public.users(id) ON DELETE CASCADE;


--
-- Name: bot_freezes bot_freezes_frozen_by_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.bot_freezes
    ADD CONSTRAINT bot_freezes_frozen_by_fkey FOREIGN KEY (frozen_by) REFERENCES public.users(id) ON DELETE SET NULL;


--
-- Name: bot_freezes bot_freezes_org_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.bot_freezes
    ADD CONSTRAINT bot_freezes_org_id_fkey FOREIGN KEY (org_id) REFERENCES public.organizations(id) ON DELETE CASCADE;


--
-- Name: branch_protections branch_protections_repo_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.branch_protections
    ADD CONSTRAINT branch_protections_repo_id_fkey FOREIGN KEY (repo_id) REFERENCES public.repositories(id) ON DELETE CASCADE;


--
-- Name: comments comments_author_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.comments
    ADD CONSTRAINT comments_author_id_fkey FOREIGN KEY (author_id) REFERENCES public.users(id) ON DELETE SET NULL;


--
-- Name: comments comments_repo_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.comments
    ADD CONSTRAINT comments_repo_id_fkey FOREIGN KEY (repo_id) REFERENCES public.repositories(id) ON DELETE CASCADE;


--
-- Name: commit_statuses commit_statuses_repo_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.commit_statuses
    ADD CONSTRAINT commit_statuses_repo_id_fkey FOREIGN KEY (repo_id) REFERENCES public.repositories(id) ON DELETE CASCADE;


--
-- Name: email_verifications email_verifications_user_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.email_verifications
    ADD CONSTRAINT email_verifications_user_id_fkey FOREIGN KEY (user_id) REFERENCES public.users(id) ON DELETE CASCADE;


--
-- Name: git_refs git_refs_repo_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.git_refs
    ADD CONSTRAINT git_refs_repo_id_fkey FOREIGN KEY (repo_id) REFERENCES public.repositories(id) ON DELETE CASCADE;


--
-- Name: instance_logos instance_logos_blob_sha256_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.instance_logos
    ADD CONSTRAINT instance_logos_blob_sha256_fkey FOREIGN KEY (blob_sha256) REFERENCES public.blobs(sha256);


--
-- Name: instance_logos instance_logos_updated_by_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.instance_logos
    ADD CONSTRAINT instance_logos_updated_by_fkey FOREIGN KEY (updated_by) REFERENCES public.users(id) ON DELETE SET NULL;


--
-- Name: invitations invitations_created_by_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.invitations
    ADD CONSTRAINT invitations_created_by_fkey FOREIGN KEY (created_by) REFERENCES public.users(id) ON DELETE SET NULL;


--
-- Name: invitations invitations_used_by_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.invitations
    ADD CONSTRAINT invitations_used_by_fkey FOREIGN KEY (used_by) REFERENCES public.users(id) ON DELETE SET NULL;


--
-- Name: issue_assignees issue_assignees_issue_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.issue_assignees
    ADD CONSTRAINT issue_assignees_issue_id_fkey FOREIGN KEY (issue_id) REFERENCES public.issues(id) ON DELETE CASCADE;


--
-- Name: issue_assignees issue_assignees_user_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.issue_assignees
    ADD CONSTRAINT issue_assignees_user_id_fkey FOREIGN KEY (user_id) REFERENCES public.users(id) ON DELETE CASCADE;


--
-- Name: issue_labels issue_labels_issue_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.issue_labels
    ADD CONSTRAINT issue_labels_issue_id_fkey FOREIGN KEY (issue_id) REFERENCES public.issues(id) ON DELETE CASCADE;


--
-- Name: issue_labels issue_labels_label_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.issue_labels
    ADD CONSTRAINT issue_labels_label_id_fkey FOREIGN KEY (label_id) REFERENCES public.labels(id) ON DELETE CASCADE;


--
-- Name: issues issues_author_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.issues
    ADD CONSTRAINT issues_author_id_fkey FOREIGN KEY (author_id) REFERENCES public.users(id) ON DELETE SET NULL;


--
-- Name: issues issues_milestone_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.issues
    ADD CONSTRAINT issues_milestone_id_fkey FOREIGN KEY (milestone_id) REFERENCES public.milestones(id) ON DELETE SET NULL;


--
-- Name: issues issues_repo_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.issues
    ADD CONSTRAINT issues_repo_id_fkey FOREIGN KEY (repo_id) REFERENCES public.repositories(id) ON DELETE CASCADE;


--
-- Name: labels labels_repo_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.labels
    ADD CONSTRAINT labels_repo_id_fkey FOREIGN KEY (repo_id) REFERENCES public.repositories(id) ON DELETE CASCADE;


--
-- Name: login_challenges login_challenges_user_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.login_challenges
    ADD CONSTRAINT login_challenges_user_id_fkey FOREIGN KEY (user_id) REFERENCES public.users(id) ON DELETE CASCADE;


--
-- Name: merge_requests merge_requests_author_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.merge_requests
    ADD CONSTRAINT merge_requests_author_id_fkey FOREIGN KEY (author_id) REFERENCES public.users(id) ON DELETE SET NULL;


--
-- Name: merge_requests merge_requests_auto_merge_by_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.merge_requests
    ADD CONSTRAINT merge_requests_auto_merge_by_fkey FOREIGN KEY (auto_merge_by) REFERENCES public.users(id) ON DELETE SET NULL;


--
-- Name: merge_requests merge_requests_merged_by_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.merge_requests
    ADD CONSTRAINT merge_requests_merged_by_fkey FOREIGN KEY (merged_by) REFERENCES public.users(id) ON DELETE SET NULL;


--
-- Name: merge_requests merge_requests_milestone_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.merge_requests
    ADD CONSTRAINT merge_requests_milestone_id_fkey FOREIGN KEY (milestone_id) REFERENCES public.milestones(id) ON DELETE SET NULL;


--
-- Name: merge_requests merge_requests_repo_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.merge_requests
    ADD CONSTRAINT merge_requests_repo_id_fkey FOREIGN KEY (repo_id) REFERENCES public.repositories(id) ON DELETE CASCADE;


--
-- Name: merge_requests merge_requests_source_repo_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.merge_requests
    ADD CONSTRAINT merge_requests_source_repo_id_fkey FOREIGN KEY (source_repo_id) REFERENCES public.repositories(id) ON DELETE SET NULL;


--
-- Name: milestones milestones_repo_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.milestones
    ADD CONSTRAINT milestones_repo_id_fkey FOREIGN KEY (repo_id) REFERENCES public.repositories(id) ON DELETE CASCADE;


--
-- Name: mr_assignees mr_assignees_mr_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.mr_assignees
    ADD CONSTRAINT mr_assignees_mr_id_fkey FOREIGN KEY (mr_id) REFERENCES public.merge_requests(id) ON DELETE CASCADE;


--
-- Name: mr_assignees mr_assignees_user_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.mr_assignees
    ADD CONSTRAINT mr_assignees_user_id_fkey FOREIGN KEY (user_id) REFERENCES public.users(id) ON DELETE CASCADE;


--
-- Name: mr_change_aliases mr_change_aliases_bound_by_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.mr_change_aliases
    ADD CONSTRAINT mr_change_aliases_bound_by_fkey FOREIGN KEY (bound_by) REFERENCES public.users(id) ON DELETE SET NULL;


--
-- Name: mr_change_aliases mr_change_aliases_change_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.mr_change_aliases
    ADD CONSTRAINT mr_change_aliases_change_id_fkey FOREIGN KEY (change_id) REFERENCES public.mr_changes(id) ON DELETE CASCADE;


--
-- Name: mr_change_aliases mr_change_aliases_mr_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.mr_change_aliases
    ADD CONSTRAINT mr_change_aliases_mr_id_fkey FOREIGN KEY (mr_id) REFERENCES public.merge_requests(id) ON DELETE CASCADE;


--
-- Name: mr_change_versions mr_change_versions_change_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.mr_change_versions
    ADD CONSTRAINT mr_change_versions_change_id_fkey FOREIGN KEY (change_id) REFERENCES public.mr_changes(id) ON DELETE CASCADE;


--
-- Name: mr_change_versions mr_change_versions_revision_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.mr_change_versions
    ADD CONSTRAINT mr_change_versions_revision_id_fkey FOREIGN KEY (revision_id) REFERENCES public.mr_revisions(id) ON DELETE CASCADE;


--
-- Name: mr_changes mr_changes_mr_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.mr_changes
    ADD CONSTRAINT mr_changes_mr_id_fkey FOREIGN KEY (mr_id) REFERENCES public.merge_requests(id) ON DELETE CASCADE;


--
-- Name: mr_checkouts mr_checkouts_mr_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.mr_checkouts
    ADD CONSTRAINT mr_checkouts_mr_id_fkey FOREIGN KEY (mr_id) REFERENCES public.merge_requests(id) ON DELETE CASCADE;


--
-- Name: mr_checkouts mr_checkouts_user_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.mr_checkouts
    ADD CONSTRAINT mr_checkouts_user_id_fkey FOREIGN KEY (user_id) REFERENCES public.users(id) ON DELETE CASCADE;


--
-- Name: mr_labels mr_labels_label_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.mr_labels
    ADD CONSTRAINT mr_labels_label_id_fkey FOREIGN KEY (label_id) REFERENCES public.labels(id) ON DELETE CASCADE;


--
-- Name: mr_labels mr_labels_mr_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.mr_labels
    ADD CONSTRAINT mr_labels_mr_id_fkey FOREIGN KEY (mr_id) REFERENCES public.merge_requests(id) ON DELETE CASCADE;


--
-- Name: mr_ledger mr_ledger_actor_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.mr_ledger
    ADD CONSTRAINT mr_ledger_actor_id_fkey FOREIGN KEY (actor_id) REFERENCES public.users(id) ON DELETE SET NULL;


--
-- Name: mr_ledger mr_ledger_mr_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.mr_ledger
    ADD CONSTRAINT mr_ledger_mr_id_fkey FOREIGN KEY (mr_id) REFERENCES public.merge_requests(id) ON DELETE CASCADE;


--
-- Name: mr_review_viewed_files mr_review_viewed_files_review_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.mr_review_viewed_files
    ADD CONSTRAINT mr_review_viewed_files_review_id_fkey FOREIGN KEY (review_id) REFERENCES public.mr_reviews(id) ON DELETE CASCADE;


--
-- Name: mr_reviewers mr_reviewers_mr_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.mr_reviewers
    ADD CONSTRAINT mr_reviewers_mr_id_fkey FOREIGN KEY (mr_id) REFERENCES public.merge_requests(id) ON DELETE CASCADE;


--
-- Name: mr_reviewers mr_reviewers_requested_by_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.mr_reviewers
    ADD CONSTRAINT mr_reviewers_requested_by_fkey FOREIGN KEY (requested_by) REFERENCES public.users(id) ON DELETE SET NULL;


--
-- Name: mr_reviewers mr_reviewers_team_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.mr_reviewers
    ADD CONSTRAINT mr_reviewers_team_id_fkey FOREIGN KEY (team_id) REFERENCES public.teams(id) ON DELETE CASCADE;


--
-- Name: mr_reviewers mr_reviewers_user_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.mr_reviewers
    ADD CONSTRAINT mr_reviewers_user_id_fkey FOREIGN KEY (user_id) REFERENCES public.users(id) ON DELETE CASCADE;


--
-- Name: mr_reviews mr_reviews_mr_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.mr_reviews
    ADD CONSTRAINT mr_reviews_mr_id_fkey FOREIGN KEY (mr_id) REFERENCES public.merge_requests(id) ON DELETE CASCADE;


--
-- Name: mr_reviews mr_reviews_reviewer_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.mr_reviews
    ADD CONSTRAINT mr_reviews_reviewer_id_fkey FOREIGN KEY (reviewer_id) REFERENCES public.users(id) ON DELETE SET NULL;


--
-- Name: mr_revisions mr_revisions_mr_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.mr_revisions
    ADD CONSTRAINT mr_revisions_mr_id_fkey FOREIGN KEY (mr_id) REFERENCES public.merge_requests(id) ON DELETE CASCADE;


--
-- Name: mr_revisions mr_revisions_pushed_by_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.mr_revisions
    ADD CONSTRAINT mr_revisions_pushed_by_fkey FOREIGN KEY (pushed_by) REFERENCES public.users(id) ON DELETE SET NULL;


--
-- Name: mr_thread_comments mr_thread_comments_author_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.mr_thread_comments
    ADD CONSTRAINT mr_thread_comments_author_id_fkey FOREIGN KEY (author_id) REFERENCES public.users(id) ON DELETE SET NULL;


--
-- Name: mr_thread_comments mr_thread_comments_thread_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.mr_thread_comments
    ADD CONSTRAINT mr_thread_comments_thread_id_fkey FOREIGN KEY (thread_id) REFERENCES public.mr_threads(id) ON DELETE CASCADE;


--
-- Name: mr_thread_findings mr_thread_findings_mr_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.mr_thread_findings
    ADD CONSTRAINT mr_thread_findings_mr_id_fkey FOREIGN KEY (mr_id) REFERENCES public.merge_requests(id) ON DELETE CASCADE;


--
-- Name: mr_thread_findings mr_thread_findings_resolved_by_status_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.mr_thread_findings
    ADD CONSTRAINT mr_thread_findings_resolved_by_status_fkey FOREIGN KEY (resolved_by_status) REFERENCES public.commit_statuses(id) ON DELETE SET NULL;


--
-- Name: mr_thread_findings mr_thread_findings_status_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.mr_thread_findings
    ADD CONSTRAINT mr_thread_findings_status_id_fkey FOREIGN KEY (status_id) REFERENCES public.commit_statuses(id) ON DELETE SET NULL;


--
-- Name: mr_thread_findings mr_thread_findings_thread_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.mr_thread_findings
    ADD CONSTRAINT mr_thread_findings_thread_id_fkey FOREIGN KEY (thread_id) REFERENCES public.mr_threads(id) ON DELETE CASCADE;


--
-- Name: mr_threads mr_threads_addressed_by_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.mr_threads
    ADD CONSTRAINT mr_threads_addressed_by_fkey FOREIGN KEY (addressed_by) REFERENCES public.users(id) ON DELETE SET NULL;


--
-- Name: mr_threads mr_threads_author_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.mr_threads
    ADD CONSTRAINT mr_threads_author_id_fkey FOREIGN KEY (author_id) REFERENCES public.users(id) ON DELETE SET NULL;


--
-- Name: mr_threads mr_threads_mr_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.mr_threads
    ADD CONSTRAINT mr_threads_mr_id_fkey FOREIGN KEY (mr_id) REFERENCES public.merge_requests(id) ON DELETE CASCADE;


--
-- Name: mr_threads mr_threads_resolved_by_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.mr_threads
    ADD CONSTRAINT mr_threads_resolved_by_fkey FOREIGN KEY (resolved_by) REFERENCES public.users(id) ON DELETE SET NULL;


--
-- Name: mr_viewed_files mr_viewed_files_mr_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.mr_viewed_files
    ADD CONSTRAINT mr_viewed_files_mr_id_fkey FOREIGN KEY (mr_id) REFERENCES public.merge_requests(id) ON DELETE CASCADE;


--
-- Name: mr_viewed_files mr_viewed_files_user_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.mr_viewed_files
    ADD CONSTRAINT mr_viewed_files_user_id_fkey FOREIGN KEY (user_id) REFERENCES public.users(id) ON DELETE CASCADE;


--
-- Name: notifications notifications_repo_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.notifications
    ADD CONSTRAINT notifications_repo_id_fkey FOREIGN KEY (repo_id) REFERENCES public.repositories(id) ON DELETE CASCADE;


--
-- Name: notifications notifications_user_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.notifications
    ADD CONSTRAINT notifications_user_id_fkey FOREIGN KEY (user_id) REFERENCES public.users(id) ON DELETE CASCADE;


--
-- Name: oidc_identities oidc_identities_user_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.oidc_identities
    ADD CONSTRAINT oidc_identities_user_id_fkey FOREIGN KEY (user_id) REFERENCES public.users(id) ON DELETE CASCADE;


--
-- Name: oidc_states oidc_states_user_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.oidc_states
    ADD CONSTRAINT oidc_states_user_id_fkey FOREIGN KEY (user_id) REFERENCES public.users(id) ON DELETE CASCADE;


--
-- Name: org_members org_members_org_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.org_members
    ADD CONSTRAINT org_members_org_id_fkey FOREIGN KEY (org_id) REFERENCES public.organizations(id) ON DELETE CASCADE;


--
-- Name: org_members org_members_user_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.org_members
    ADD CONSTRAINT org_members_user_id_fkey FOREIGN KEY (user_id) REFERENCES public.users(id) ON DELETE CASCADE;


--
-- Name: organizations organizations_avatar_blob_sha256_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.organizations
    ADD CONSTRAINT organizations_avatar_blob_sha256_fkey FOREIGN KEY (avatar_blob_sha256) REFERENCES public.blobs(sha256);


--
-- Name: organizations organizations_namespace_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.organizations
    ADD CONSTRAINT organizations_namespace_id_fkey FOREIGN KEY (namespace_id) REFERENCES public.namespaces(id);


--
-- Name: password_resets password_resets_user_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.password_resets
    ADD CONSTRAINT password_resets_user_id_fkey FOREIGN KEY (user_id) REFERENCES public.users(id) ON DELETE CASCADE;


--
-- Name: push_events push_events_pusher_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.push_events
    ADD CONSTRAINT push_events_pusher_id_fkey FOREIGN KEY (pusher_id) REFERENCES public.users(id) ON DELETE SET NULL;


--
-- Name: push_events push_events_repo_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.push_events
    ADD CONSTRAINT push_events_repo_id_fkey FOREIGN KEY (repo_id) REFERENCES public.repositories(id) ON DELETE CASCADE;


--
-- Name: recovery_codes recovery_codes_user_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.recovery_codes
    ADD CONSTRAINT recovery_codes_user_id_fkey FOREIGN KEY (user_id) REFERENCES public.users(id) ON DELETE CASCADE;


--
-- Name: ref_updates ref_updates_push_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.ref_updates
    ADD CONSTRAINT ref_updates_push_id_fkey FOREIGN KEY (push_id) REFERENCES public.push_events(id) ON DELETE CASCADE;


--
-- Name: repo_collaborators repo_collaborators_repo_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.repo_collaborators
    ADD CONSTRAINT repo_collaborators_repo_id_fkey FOREIGN KEY (repo_id) REFERENCES public.repositories(id) ON DELETE CASCADE;


--
-- Name: repo_collaborators repo_collaborators_user_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.repo_collaborators
    ADD CONSTRAINT repo_collaborators_user_id_fkey FOREIGN KEY (user_id) REFERENCES public.users(id) ON DELETE CASCADE;


--
-- Name: repo_forks repo_forks_parent_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.repo_forks
    ADD CONSTRAINT repo_forks_parent_id_fkey FOREIGN KEY (parent_id) REFERENCES public.repositories(id) ON DELETE SET NULL;


--
-- Name: repo_forks repo_forks_repo_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.repo_forks
    ADD CONSTRAINT repo_forks_repo_id_fkey FOREIGN KEY (repo_id) REFERENCES public.repositories(id) ON DELETE CASCADE;


--
-- Name: repo_forks repo_forks_requested_by_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.repo_forks
    ADD CONSTRAINT repo_forks_requested_by_fkey FOREIGN KEY (requested_by) REFERENCES public.users(id) ON DELETE SET NULL;


--
-- Name: repo_imports repo_imports_repo_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.repo_imports
    ADD CONSTRAINT repo_imports_repo_id_fkey FOREIGN KEY (repo_id) REFERENCES public.repositories(id) ON DELETE CASCADE;


--
-- Name: repo_imports repo_imports_requested_by_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.repo_imports
    ADD CONSTRAINT repo_imports_requested_by_fkey FOREIGN KEY (requested_by) REFERENCES public.users(id) ON DELETE SET NULL;


--
-- Name: repo_redirects repo_redirects_namespace_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.repo_redirects
    ADD CONSTRAINT repo_redirects_namespace_id_fkey FOREIGN KEY (namespace_id) REFERENCES public.namespaces(id);


--
-- Name: repo_redirects repo_redirects_repo_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.repo_redirects
    ADD CONSTRAINT repo_redirects_repo_id_fkey FOREIGN KEY (repo_id) REFERENCES public.repositories(id) ON DELETE CASCADE;


--
-- Name: repo_team_access repo_team_access_repo_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.repo_team_access
    ADD CONSTRAINT repo_team_access_repo_id_fkey FOREIGN KEY (repo_id) REFERENCES public.repositories(id) ON DELETE CASCADE;


--
-- Name: repo_team_access repo_team_access_team_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.repo_team_access
    ADD CONSTRAINT repo_team_access_team_id_fkey FOREIGN KEY (team_id) REFERENCES public.teams(id) ON DELETE CASCADE;


--
-- Name: repositories repositories_fork_parent_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.repositories
    ADD CONSTRAINT repositories_fork_parent_id_fkey FOREIGN KEY (fork_parent_id) REFERENCES public.repositories(id) ON DELETE SET NULL;


--
-- Name: repositories repositories_namespace_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.repositories
    ADD CONSTRAINT repositories_namespace_id_fkey FOREIGN KEY (namespace_id) REFERENCES public.namespaces(id);


--
-- Name: secret_allowances secret_allowances_created_by_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.secret_allowances
    ADD CONSTRAINT secret_allowances_created_by_fkey FOREIGN KEY (created_by) REFERENCES public.users(id) ON DELETE SET NULL;


--
-- Name: secret_allowances secret_allowances_repo_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.secret_allowances
    ADD CONSTRAINT secret_allowances_repo_id_fkey FOREIGN KEY (repo_id) REFERENCES public.repositories(id) ON DELETE CASCADE;


--
-- Name: secret_rejections secret_rejections_repo_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.secret_rejections
    ADD CONSTRAINT secret_rejections_repo_id_fkey FOREIGN KEY (repo_id) REFERENCES public.repositories(id) ON DELETE CASCADE;


--
-- Name: sessions sessions_user_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.sessions
    ADD CONSTRAINT sessions_user_id_fkey FOREIGN KEY (user_id) REFERENCES public.users(id) ON DELETE CASCADE;


--
-- Name: ssh_keys ssh_keys_added_by_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.ssh_keys
    ADD CONSTRAINT ssh_keys_added_by_fkey FOREIGN KEY (added_by) REFERENCES public.users(id) ON DELETE SET NULL;


--
-- Name: ssh_keys ssh_keys_repo_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.ssh_keys
    ADD CONSTRAINT ssh_keys_repo_id_fkey FOREIGN KEY (repo_id) REFERENCES public.repositories(id) ON DELETE CASCADE;


--
-- Name: ssh_keys ssh_keys_user_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.ssh_keys
    ADD CONSTRAINT ssh_keys_user_id_fkey FOREIGN KEY (user_id) REFERENCES public.users(id) ON DELETE CASCADE;


--
-- Name: status_annotations status_annotations_status_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.status_annotations
    ADD CONSTRAINT status_annotations_status_id_fkey FOREIGN KEY (status_id) REFERENCES public.commit_statuses(id) ON DELETE CASCADE;


--
-- Name: subscriptions subscriptions_repo_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.subscriptions
    ADD CONSTRAINT subscriptions_repo_id_fkey FOREIGN KEY (repo_id) REFERENCES public.repositories(id) ON DELETE CASCADE;


--
-- Name: subscriptions subscriptions_user_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.subscriptions
    ADD CONSTRAINT subscriptions_user_id_fkey FOREIGN KEY (user_id) REFERENCES public.users(id) ON DELETE CASCADE;


--
-- Name: team_members team_members_team_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.team_members
    ADD CONSTRAINT team_members_team_id_fkey FOREIGN KEY (team_id) REFERENCES public.teams(id) ON DELETE CASCADE;


--
-- Name: team_members team_members_user_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.team_members
    ADD CONSTRAINT team_members_user_id_fkey FOREIGN KEY (user_id) REFERENCES public.users(id) ON DELETE CASCADE;


--
-- Name: teams teams_org_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.teams
    ADD CONSTRAINT teams_org_id_fkey FOREIGN KEY (org_id) REFERENCES public.organizations(id) ON DELETE CASCADE;


--
-- Name: timeline_events timeline_events_actor_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.timeline_events
    ADD CONSTRAINT timeline_events_actor_id_fkey FOREIGN KEY (actor_id) REFERENCES public.users(id) ON DELETE SET NULL;


--
-- Name: timeline_events timeline_events_repo_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.timeline_events
    ADD CONSTRAINT timeline_events_repo_id_fkey FOREIGN KEY (repo_id) REFERENCES public.repositories(id) ON DELETE CASCADE;


--
-- Name: totp_credentials totp_credentials_key_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.totp_credentials
    ADD CONSTRAINT totp_credentials_key_id_fkey FOREIGN KEY (key_id) REFERENCES public.encryption_keys(key_id);


--
-- Name: totp_credentials totp_credentials_user_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.totp_credentials
    ADD CONSTRAINT totp_credentials_user_id_fkey FOREIGN KEY (user_id) REFERENCES public.users(id) ON DELETE CASCADE;


--
-- Name: user_emails user_emails_user_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.user_emails
    ADD CONSTRAINT user_emails_user_id_fkey FOREIGN KEY (user_id) REFERENCES public.users(id) ON DELETE CASCADE;


--
-- Name: users users_avatar_blob_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.users
    ADD CONSTRAINT users_avatar_blob_fkey FOREIGN KEY (avatar_blob_sha256) REFERENCES public.blobs(sha256);


--
-- Name: users users_namespace_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.users
    ADD CONSTRAINT users_namespace_id_fkey FOREIGN KEY (namespace_id) REFERENCES public.namespaces(id);


--
-- Name: users users_stopped_by_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.users
    ADD CONSTRAINT users_stopped_by_fkey FOREIGN KEY (stopped_by) REFERENCES public.users(id) ON DELETE SET NULL;


--
-- Name: webauthn_ceremonies webauthn_ceremonies_user_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.webauthn_ceremonies
    ADD CONSTRAINT webauthn_ceremonies_user_id_fkey FOREIGN KEY (user_id) REFERENCES public.users(id) ON DELETE CASCADE;


--
-- Name: webauthn_credentials webauthn_credentials_user_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.webauthn_credentials
    ADD CONSTRAINT webauthn_credentials_user_id_fkey FOREIGN KEY (user_id) REFERENCES public.users(id) ON DELETE CASCADE;


--
-- Name: webhook_attempts webhook_attempts_delivery_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.webhook_attempts
    ADD CONSTRAINT webhook_attempts_delivery_id_fkey FOREIGN KEY (delivery_id) REFERENCES public.webhook_deliveries(id) ON DELETE CASCADE;


--
-- Name: webhook_deliveries webhook_deliveries_redelivery_of_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.webhook_deliveries
    ADD CONSTRAINT webhook_deliveries_redelivery_of_fkey FOREIGN KEY (redelivery_of) REFERENCES public.webhook_deliveries(id) ON DELETE SET NULL;


--
-- Name: webhook_deliveries webhook_deliveries_webhook_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.webhook_deliveries
    ADD CONSTRAINT webhook_deliveries_webhook_id_fkey FOREIGN KEY (webhook_id) REFERENCES public.webhooks(id) ON DELETE CASCADE;


--
-- Name: webhooks webhooks_key_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.webhooks
    ADD CONSTRAINT webhooks_key_id_fkey FOREIGN KEY (key_id) REFERENCES public.encryption_keys(key_id);


--
-- Name: webhooks webhooks_org_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.webhooks
    ADD CONSTRAINT webhooks_org_id_fkey FOREIGN KEY (org_id) REFERENCES public.organizations(id) ON DELETE CASCADE;


--
-- Name: webhooks webhooks_principal_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.webhooks
    ADD CONSTRAINT webhooks_principal_id_fkey FOREIGN KEY (principal_id) REFERENCES public.users(id) ON DELETE SET NULL;


--
-- Name: webhooks webhooks_repo_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.webhooks
    ADD CONSTRAINT webhooks_repo_id_fkey FOREIGN KEY (repo_id) REFERENCES public.repositories(id) ON DELETE CASCADE;


--
-- PostgreSQL database dump complete
--

\unrestrict zGovOBUUgcU9BZAoWthlslC0y4gyHHbL9qwH2bM4dSts4J8eALgVO2u9bhRe27w

