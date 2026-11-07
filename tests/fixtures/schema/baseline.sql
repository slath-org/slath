-- SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- Schema and data at the upgrade baseline (schema 100, before any 0.3.0
-- migration): the data of the 0.2.0 fixture in the baseline schema. The
-- upgrade tests start from it until a release since 0.3.0 has its own
-- fixture. Users: alice, bob, root-admin (password "slath test password").
-- Don't edit.

--
-- PostgreSQL database dump
--

\restrict VQtY69eNhzPfID1OJYKxRRUsB7hpgXanBD3xQCJFdZXAgpNyJZPYhngqMENtArr

-- Dumped from database version 18.6 (Debian 18.6-1.pgdg13+2)
-- Dumped by pg_dump version 18.6 (Debian 18.6-1.pgdg13+2)

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
-- Name: EXTENSION citext; Type: COMMENT; Schema: -; Owner: 
--

COMMENT ON EXTENSION citext IS 'data type for case-insensitive character strings';


--
-- Name: pg_trgm; Type: EXTENSION; Schema: -; Owner: -
--

CREATE EXTENSION IF NOT EXISTS pg_trgm WITH SCHEMA public;


--
-- Name: EXTENSION pg_trgm; Type: COMMENT; Schema: -; Owner: 
--

COMMENT ON EXTENSION pg_trgm IS 'text similarity measurement and index searching based on trigrams';


--
-- Name: audit_events_append_only(); Type: FUNCTION; Schema: public; Owner: slath
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


ALTER FUNCTION public.audit_events_append_only() OWNER TO slath;

--
-- Name: slath_attachment_blob_ref(); Type: FUNCTION; Schema: public; Owner: slath
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


ALTER FUNCTION public.slath_attachment_blob_ref() OWNER TO slath;

--
-- Name: slath_avatar_blob_ref(); Type: FUNCTION; Schema: public; Owner: slath
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


ALTER FUNCTION public.slath_avatar_blob_ref() OWNER TO slath;

--
-- Name: slath_blob_recount(); Type: FUNCTION; Schema: public; Owner: slath
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
    ), total AS (SELECT sha, sum(n)::int AS n FROM uses GROUP BY sha)
    UPDATE blobs b
       SET refcount = COALESCE(t.n, 0),
           unreferenced_since = CASE WHEN COALESCE(t.n, 0) = 0
                                     THEN COALESCE(b.unreferenced_since, now()) END
      FROM blobs b2 LEFT JOIN total t ON t.sha = b2.sha256
     WHERE b.sha256 = b2.sha256;
$$;


ALTER FUNCTION public.slath_blob_recount() OWNER TO slath;

--
-- Name: slath_blob_ref(text, integer); Type: FUNCTION; Schema: public; Owner: slath
--

CREATE FUNCTION public.slath_blob_ref(sha text, delta integer) RETURNS void
    LANGUAGE sql
    AS $$
    UPDATE blobs
       SET refcount = refcount + delta,
           unreferenced_since = CASE WHEN refcount + delta = 0 THEN now() ELSE NULL END
     WHERE sha256 = sha;
$$;


ALTER FUNCTION public.slath_blob_ref(sha text, delta integer) OWNER TO slath;

--
-- Name: slath_can_read(uuid, uuid); Type: FUNCTION; Schema: public; Owner: slath
--

CREATE FUNCTION public.slath_can_read(repo uuid, viewer uuid) RETURNS boolean
    LANGUAGE sql STABLE
    AS $$
    SELECT viewer IS NOT NULL AND EXISTS (
        SELECT 1 FROM repositories r
        LEFT JOIN organizations o ON o.namespace_id = r.namespace_id
        WHERE r.id = repo
          AND NOT (COALESCE(o.require_mfa, false) AND NOT slath_has_mfa(viewer))
          AND (
               EXISTS (SELECT 1 FROM repo_collaborators c WHERE c.repo_id = r.id AND c.user_id = viewer)
            OR EXISTS (SELECT 1 FROM users u WHERE u.id = viewer AND u.namespace_id = r.namespace_id)
            OR EXISTS (SELECT 1 FROM org_members m WHERE m.org_id = o.id AND m.user_id = viewer
                         AND (m.role = 'owner' OR o.default_member_role <> 'none'))
            OR EXISTS (SELECT 1 FROM repo_team_access a JOIN team_members tm ON tm.team_id = a.team_id
                       WHERE a.repo_id = r.id AND tm.user_id = viewer)
          )
    )
$$;


ALTER FUNCTION public.slath_can_read(repo uuid, viewer uuid) OWNER TO slath;

--
-- Name: slath_has_mfa(uuid); Type: FUNCTION; Schema: public; Owner: slath
--

CREATE FUNCTION public.slath_has_mfa(viewer uuid) RETURNS boolean
    LANGUAGE sql STABLE
    AS $$
    SELECT EXISTS (SELECT 1 FROM totp_credentials WHERE user_id = viewer AND confirmed)
        OR EXISTS (SELECT 1 FROM webauthn_credentials WHERE user_id = viewer)
$$;


ALTER FUNCTION public.slath_has_mfa(viewer uuid) OWNER TO slath;

SET default_tablespace = '';

SET default_table_access_method = heap;

--
-- Name: _sqlx_migrations; Type: TABLE; Schema: public; Owner: slath
--

CREATE TABLE public._sqlx_migrations (
    version bigint NOT NULL,
    description text NOT NULL,
    installed_on timestamp with time zone DEFAULT now() NOT NULL,
    success boolean NOT NULL,
    checksum bytea NOT NULL,
    execution_time bigint NOT NULL
);


ALTER TABLE public._sqlx_migrations OWNER TO slath;

--
-- Name: access_tokens; Type: TABLE; Schema: public; Owner: slath
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


ALTER TABLE public.access_tokens OWNER TO slath;

--
-- Name: attachments; Type: TABLE; Schema: public; Owner: slath
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


ALTER TABLE public.attachments OWNER TO slath;

--
-- Name: audit_events; Type: TABLE; Schema: public; Owner: slath
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


ALTER TABLE public.audit_events OWNER TO slath;

--
-- Name: background_migrations; Type: TABLE; Schema: public; Owner: slath
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


ALTER TABLE public.background_migrations OWNER TO slath;

--
-- Name: blobs; Type: TABLE; Schema: public; Owner: slath
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


ALTER TABLE public.blobs OWNER TO slath;

--
-- Name: branch_protections; Type: TABLE; Schema: public; Owner: slath
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
    CONSTRAINT branch_protections_pattern_check CHECK (((length(pattern) >= 1) AND (length(pattern) <= 255))),
    CONSTRAINT branch_protections_push_role_check CHECK ((push_role = ANY (ARRAY['write'::text, 'maintain'::text, 'admin'::text, 'nobody'::text]))),
    CONSTRAINT branch_protections_required_approvals_check CHECK (((required_approvals >= 0) AND (required_approvals <= 20)))
);


ALTER TABLE public.branch_protections OWNER TO slath;

--
-- Name: comments; Type: TABLE; Schema: public; Owner: slath
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
    CONSTRAINT comments_subject_kind_check CHECK ((subject_kind = ANY (ARRAY['issue'::text, 'merge_request'::text])))
);


ALTER TABLE public.comments OWNER TO slath;

--
-- Name: email_verifications; Type: TABLE; Schema: public; Owner: slath
--

CREATE TABLE public.email_verifications (
    token_hash bytea NOT NULL,
    user_id uuid NOT NULL,
    email public.citext NOT NULL,
    expires_at timestamp with time zone NOT NULL,
    used_at timestamp with time zone,
    created_at timestamp with time zone DEFAULT now() NOT NULL
);


ALTER TABLE public.email_verifications OWNER TO slath;

--
-- Name: encryption_keys; Type: TABLE; Schema: public; Owner: slath
--

CREATE TABLE public.encryption_keys (
    key_id text NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    retired_at timestamp with time zone,
    CONSTRAINT encryption_keys_key_id_check CHECK ((key_id ~ '^[0-9a-f]{8}$'::text))
);


ALTER TABLE public.encryption_keys OWNER TO slath;

--
-- Name: git_refs; Type: TABLE; Schema: public; Owner: slath
--

CREATE TABLE public.git_refs (
    repo_id uuid NOT NULL,
    name bytea NOT NULL,
    oid text NOT NULL,
    updated_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT git_refs_oid_check CHECK ((oid ~ '^[0-9a-f]{40}$'::text))
);


ALTER TABLE public.git_refs OWNER TO slath;

--
-- Name: idempotency_keys; Type: TABLE; Schema: public; Owner: slath
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


ALTER TABLE public.idempotency_keys OWNER TO slath;

--
-- Name: instance_settings; Type: TABLE; Schema: public; Owner: slath
--

CREATE TABLE public.instance_settings (
    key text NOT NULL,
    value jsonb NOT NULL,
    updated_at timestamp with time zone DEFAULT now() NOT NULL
);


ALTER TABLE public.instance_settings OWNER TO slath;

--
-- Name: invitations; Type: TABLE; Schema: public; Owner: slath
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


ALTER TABLE public.invitations OWNER TO slath;

--
-- Name: issue_assignees; Type: TABLE; Schema: public; Owner: slath
--

CREATE TABLE public.issue_assignees (
    issue_id uuid NOT NULL,
    user_id uuid NOT NULL
);


ALTER TABLE public.issue_assignees OWNER TO slath;

--
-- Name: issue_labels; Type: TABLE; Schema: public; Owner: slath
--

CREATE TABLE public.issue_labels (
    issue_id uuid NOT NULL,
    label_id uuid NOT NULL
);


ALTER TABLE public.issue_labels OWNER TO slath;

--
-- Name: issues; Type: TABLE; Schema: public; Owner: slath
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
    CONSTRAINT issues_number_check CHECK ((number > 0)),
    CONSTRAINT issues_state_check CHECK ((state = ANY (ARRAY['open'::text, 'closed'::text]))),
    CONSTRAINT issues_state_reason_check CHECK ((state_reason = ANY (ARRAY['completed'::text, 'not_planned'::text, 'duplicate'::text]))),
    CONSTRAINT issues_title_check CHECK (((length(title) >= 1) AND (length(title) <= 256)))
);


ALTER TABLE public.issues OWNER TO slath;

--
-- Name: jobs; Type: TABLE; Schema: public; Owner: slath
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


ALTER TABLE public.jobs OWNER TO slath;

--
-- Name: jobs_id_seq; Type: SEQUENCE; Schema: public; Owner: slath
--

CREATE SEQUENCE public.jobs_id_seq
    START WITH 1
    INCREMENT BY 1
    NO MINVALUE
    NO MAXVALUE
    CACHE 1;


ALTER SEQUENCE public.jobs_id_seq OWNER TO slath;

--
-- Name: jobs_id_seq; Type: SEQUENCE OWNED BY; Schema: public; Owner: slath
--

ALTER SEQUENCE public.jobs_id_seq OWNED BY public.jobs.id;


--
-- Name: labels; Type: TABLE; Schema: public; Owner: slath
--

CREATE TABLE public.labels (
    id uuid NOT NULL,
    repo_id uuid NOT NULL,
    name public.citext NOT NULL,
    color character(6) NOT NULL,
    description text,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT labels_color_check CHECK ((color ~ '^[0-9a-f]{6}$'::text)),
    CONSTRAINT labels_name_check CHECK (((length((name)::text) >= 1) AND (length((name)::text) <= 50)))
);


ALTER TABLE public.labels OWNER TO slath;

--
-- Name: login_attempts; Type: TABLE; Schema: public; Owner: slath
--

CREATE TABLE public.login_attempts (
    key text NOT NULL,
    window_start timestamp with time zone NOT NULL,
    count integer DEFAULT 0 NOT NULL,
    CONSTRAINT login_attempts_key_check CHECK ((length(key) <= 200))
);


ALTER TABLE public.login_attempts OWNER TO slath;

--
-- Name: login_challenges; Type: TABLE; Schema: public; Owner: slath
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


ALTER TABLE public.login_challenges OWNER TO slath;

--
-- Name: milestones; Type: TABLE; Schema: public; Owner: slath
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
    CONSTRAINT milestones_state_check CHECK ((state = ANY (ARRAY['open'::text, 'closed'::text]))),
    CONSTRAINT milestones_title_check CHECK (((length((title)::text) >= 1) AND (length((title)::text) <= 100)))
);


ALTER TABLE public.milestones OWNER TO slath;

--
-- Name: namespaces; Type: TABLE; Schema: public; Owner: slath
--

CREATE TABLE public.namespaces (
    id uuid NOT NULL,
    name public.citext NOT NULL,
    kind text NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT namespaces_kind_check CHECK ((kind = ANY (ARRAY['user'::text, 'org'::text])))
);


ALTER TABLE public.namespaces OWNER TO slath;

--
-- Name: notifications; Type: TABLE; Schema: public; Owner: slath
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


ALTER TABLE public.notifications OWNER TO slath;

--
-- Name: oidc_identities; Type: TABLE; Schema: public; Owner: slath
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


ALTER TABLE public.oidc_identities OWNER TO slath;

--
-- Name: oidc_pending; Type: TABLE; Schema: public; Owner: slath
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


ALTER TABLE public.oidc_pending OWNER TO slath;

--
-- Name: oidc_states; Type: TABLE; Schema: public; Owner: slath
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


ALTER TABLE public.oidc_states OWNER TO slath;

--
-- Name: org_members; Type: TABLE; Schema: public; Owner: slath
--

CREATE TABLE public.org_members (
    org_id uuid NOT NULL,
    user_id uuid NOT NULL,
    role text NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT org_members_role_check CHECK ((role = ANY (ARRAY['owner'::text, 'member'::text])))
);


ALTER TABLE public.org_members OWNER TO slath;

--
-- Name: organizations; Type: TABLE; Schema: public; Owner: slath
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


ALTER TABLE public.organizations OWNER TO slath;

--
-- Name: password_resets; Type: TABLE; Schema: public; Owner: slath
--

CREATE TABLE public.password_resets (
    token_hash bytea NOT NULL,
    user_id uuid NOT NULL,
    expires_at timestamp with time zone NOT NULL,
    used_at timestamp with time zone,
    created_at timestamp with time zone DEFAULT now() NOT NULL
);


ALTER TABLE public.password_resets OWNER TO slath;

--
-- Name: push_events; Type: TABLE; Schema: public; Owner: slath
--

CREATE TABLE public.push_events (
    id uuid NOT NULL,
    repo_id uuid NOT NULL,
    pusher_id uuid,
    transport text NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT push_events_transport_check CHECK ((transport = ANY (ARRAY['http'::text, 'ssh'::text, 'internal'::text, 'reconcile'::text])))
);


ALTER TABLE public.push_events OWNER TO slath;

--
-- Name: recovery_codes; Type: TABLE; Schema: public; Owner: slath
--

CREATE TABLE public.recovery_codes (
    id uuid NOT NULL,
    user_id uuid NOT NULL,
    code_hash text NOT NULL,
    used_at timestamp with time zone,
    created_at timestamp with time zone DEFAULT now() NOT NULL
);


ALTER TABLE public.recovery_codes OWNER TO slath;

--
-- Name: ref_updates; Type: TABLE; Schema: public; Owner: slath
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


ALTER TABLE public.ref_updates OWNER TO slath;

--
-- Name: repo_collaborators; Type: TABLE; Schema: public; Owner: slath
--

CREATE TABLE public.repo_collaborators (
    repo_id uuid NOT NULL,
    user_id uuid NOT NULL,
    role text NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT repo_collaborators_role_check CHECK ((role = ANY (ARRAY['read'::text, 'write'::text, 'maintain'::text, 'admin'::text])))
);


ALTER TABLE public.repo_collaborators OWNER TO slath;

--
-- Name: repo_imports; Type: TABLE; Schema: public; Owner: slath
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
    CONSTRAINT repo_imports_state_check CHECK ((state = ANY (ARRAY['queued'::text, 'running'::text, 'done'::text, 'failed'::text])))
);


ALTER TABLE public.repo_imports OWNER TO slath;

--
-- Name: repo_redirects; Type: TABLE; Schema: public; Owner: slath
--

CREATE TABLE public.repo_redirects (
    namespace_id uuid NOT NULL,
    name public.citext NOT NULL,
    repo_id uuid NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL
);


ALTER TABLE public.repo_redirects OWNER TO slath;

--
-- Name: repo_team_access; Type: TABLE; Schema: public; Owner: slath
--

CREATE TABLE public.repo_team_access (
    repo_id uuid NOT NULL,
    team_id uuid NOT NULL,
    role text NOT NULL,
    CONSTRAINT repo_team_access_role_check CHECK ((role = ANY (ARRAY['read'::text, 'write'::text, 'maintain'::text, 'admin'::text])))
);


ALTER TABLE public.repo_team_access OWNER TO slath;

--
-- Name: repositories; Type: TABLE; Schema: public; Owner: slath
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
    CONSTRAINT repositories_archived_reason_check CHECK ((archived_reason = ANY (ARRAY['user'::text, 'lost_parent_access'::text]))),
    CONSTRAINT repositories_default_branch_check CHECK (((length(default_branch) >= 1) AND (length(default_branch) <= 255))),
    CONSTRAINT repositories_description_check CHECK ((length(description) <= 500)),
    CONSTRAINT repositories_state_check CHECK ((state = ANY (ARRAY['active'::text, 'archived'::text, 'deleting'::text]))),
    CONSTRAINT repositories_visibility_check CHECK ((visibility = ANY (ARRAY['public'::text, 'internal'::text, 'private'::text])))
);


ALTER TABLE public.repositories OWNER TO slath;

--
-- Name: reserved_names; Type: TABLE; Schema: public; Owner: slath
--

CREATE TABLE public.reserved_names (
    name public.citext NOT NULL
);


ALTER TABLE public.reserved_names OWNER TO slath;

--
-- Name: schema_info; Type: TABLE; Schema: public; Owner: slath
--

CREATE TABLE public.schema_info (
    version integer NOT NULL,
    singleton boolean DEFAULT true NOT NULL,
    CONSTRAINT schema_info_singleton_check CHECK (singleton)
);


ALTER TABLE public.schema_info OWNER TO slath;

--
-- Name: sessions; Type: TABLE; Schema: public; Owner: slath
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


ALTER TABLE public.sessions OWNER TO slath;

--
-- Name: ssh_keys; Type: TABLE; Schema: public; Owner: slath
--

CREATE TABLE public.ssh_keys (
    id uuid NOT NULL,
    user_id uuid NOT NULL,
    title text NOT NULL,
    key_type text NOT NULL,
    public_key text NOT NULL,
    fingerprint_sha256 text NOT NULL,
    last_used_at timestamp with time zone,
    created_at timestamp with time zone DEFAULT now() NOT NULL,
    CONSTRAINT ssh_keys_fingerprint_sha256_check CHECK ((fingerprint_sha256 ~ '^SHA256:[A-Za-z0-9+/]{43}$'::text)),
    CONSTRAINT ssh_keys_key_type_check CHECK ((key_type ~ '^[A-Za-z0-9.@-]+$'::text)),
    CONSTRAINT ssh_keys_public_key_check CHECK ((public_key ~ '^[A-Za-z0-9+/=]+$'::text)),
    CONSTRAINT ssh_keys_title_check CHECK (((length(title) >= 1) AND (length(title) <= 100)))
);


ALTER TABLE public.ssh_keys OWNER TO slath;

--
-- Name: subscriptions; Type: TABLE; Schema: public; Owner: slath
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


ALTER TABLE public.subscriptions OWNER TO slath;

--
-- Name: team_members; Type: TABLE; Schema: public; Owner: slath
--

CREATE TABLE public.team_members (
    team_id uuid NOT NULL,
    user_id uuid NOT NULL
);


ALTER TABLE public.team_members OWNER TO slath;

--
-- Name: teams; Type: TABLE; Schema: public; Owner: slath
--

CREATE TABLE public.teams (
    id uuid NOT NULL,
    org_id uuid NOT NULL,
    name public.citext NOT NULL,
    description text,
    created_at timestamp with time zone DEFAULT now() NOT NULL
);


ALTER TABLE public.teams OWNER TO slath;

--
-- Name: timeline_events; Type: TABLE; Schema: public; Owner: slath
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
    CONSTRAINT timeline_events_kind_check CHECK ((kind = ANY (ARRAY['closed'::text, 'reopened'::text, 'labeled'::text, 'unlabeled'::text, 'assigned'::text, 'unassigned'::text, 'milestoned'::text, 'demilestoned'::text, 'renamed'::text, 'referenced'::text]))),
    CONSTRAINT timeline_events_subject_kind_check CHECK ((subject_kind = ANY (ARRAY['issue'::text, 'merge_request'::text])))
);


ALTER TABLE public.timeline_events OWNER TO slath;

--
-- Name: totp_credentials; Type: TABLE; Schema: public; Owner: slath
--

CREATE TABLE public.totp_credentials (
    user_id uuid NOT NULL,
    secret_enc bytea NOT NULL,
    key_id text NOT NULL,
    last_used_step bigint DEFAULT 0 NOT NULL,
    confirmed boolean DEFAULT false NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL
);


ALTER TABLE public.totp_credentials OWNER TO slath;

--
-- Name: upgrade_history; Type: TABLE; Schema: public; Owner: slath
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


ALTER TABLE public.upgrade_history OWNER TO slath;

--
-- Name: users; Type: TABLE; Schema: public; Owner: slath
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
    CONSTRAINT users_bot_owner_kind_check CHECK ((bot_owner_kind = ANY (ARRAY['user'::text, 'org'::text]))),
    CONSTRAINT users_check CHECK (((kind = 'bot'::text) = ((bot_owner_kind IS NOT NULL) AND (bot_owner_id IS NOT NULL)))),
    CONSTRAINT users_check1 CHECK (((kind = 'human'::text) OR (password_hash IS NULL))),
    CONSTRAINT users_code_font_size_check CHECK (((code_font_size >= 12) AND (code_font_size <= 20))),
    CONSTRAINT users_density_check CHECK ((density = ANY (ARRAY['comfortable'::text, 'compact'::text]))),
    CONSTRAINT users_diff_view_check CHECK ((diff_view = ANY (ARRAY['unified'::text, 'split'::text]))),
    CONSTRAINT users_editor_mode_check CHECK ((editor_mode = ANY (ARRAY['wysiwyg'::text, 'source'::text]))),
    CONSTRAINT users_kind_check CHECK ((kind = ANY (ARRAY['human'::text, 'bot'::text]))),
    CONSTRAINT users_state_check CHECK ((state = ANY (ARRAY['active'::text, 'suspended'::text, 'deleted'::text]))),
    CONSTRAINT users_theme_check CHECK ((theme = ANY (ARRAY['system'::text, 'light'::text, 'dark'::text])))
);


ALTER TABLE public.users OWNER TO slath;

--
-- Name: webauthn_ceremonies; Type: TABLE; Schema: public; Owner: slath
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


ALTER TABLE public.webauthn_ceremonies OWNER TO slath;

--
-- Name: webauthn_credentials; Type: TABLE; Schema: public; Owner: slath
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


ALTER TABLE public.webauthn_credentials OWNER TO slath;

--
-- Name: jobs id; Type: DEFAULT; Schema: public; Owner: slath
--

ALTER TABLE ONLY public.jobs ALTER COLUMN id SET DEFAULT nextval('public.jobs_id_seq'::regclass);


--
-- Data for Name: _sqlx_migrations; Type: TABLE DATA; Schema: public; Owner: slath
--

COPY public._sqlx_migrations (version, description, installed_on, success, checksum, execution_time) FROM stdin;
100	baseline	2026-11-07 21:05:55.873034+00	t	\\x21982106074b838c30a089284f29ef557a0c54c5a2732106e77b2d2da90d0657938e64f7ca57d5796865202fd7c97eab	174766485
\.


--
-- Data for Name: access_tokens; Type: TABLE DATA; Schema: public; Owner: slath
--

COPY public.access_tokens (id, user_id, name, token_prefix, token_hash, scopes, repo_ids, expires_at, last_used_at, revoked_at, created_at) FROM stdin;
01a19c7d-3ded-71c5-ab25-0fba197909af	01a19c7d-3cdd-740a-ac7a-ee6477599dde	tests	FmKSsuuB	\\x0fb477d265f04dfe50c859fb1257e0bac53ddf47613a38ab22793642c0992320	{repo:write,user:read}	\N	2026-12-02 13:40:15.981044+00	2026-11-02 13:40:16.691536+00	\N	2026-11-02 13:40:15.982367+00
01a19c7d-42f6-72ea-a7eb-6491ffa8bd7b	01a19c7d-3ef9-7622-b233-a06d9df83c3e	ci	Wn61OHSm	\\xe1fe7783a8b9b9668169acd535ed5c7790ecd9c910216cc0b6bd042e58309bbd	{repo:read}	\N	2026-12-02 13:40:17.270784+00	\N	\N	2026-11-02 13:40:17.271931+00
01a19c7d-43af-70ab-9175-a75539462f8c	01a19c7d-3cdd-740a-ac7a-ee6477599dde	fixture	arhgadDg	\\x65b7f0323897a842a7daa489a294ff1c1d421524a148c545848721d629f1d870	{repo:read,issues:write}	\N	2026-12-02 13:40:17.455251+00	2026-11-02 13:40:17.474374+00	\N	2026-11-02 13:40:17.455822+00
\.


--
-- Data for Name: attachments; Type: TABLE DATA; Schema: public; Owner: slath
--

COPY public.attachments (id, repo_id, uploader_id, blob_sha256, filename, content_type, size_bytes, created_at) FROM stdin;
\.


--
-- Data for Name: audit_events; Type: TABLE DATA; Schema: public; Owner: slath
--

COPY public.audit_events (id, actor_id, actor_ip, action, target_kind, target_id, repo_id, org_id, data, created_at) FROM stdin;
01a19c7d-3ce2-77a8-8126-d3dcec6e6163	\N	\N	user.create	user	01a19c7d-3cdd-740a-ac7a-ee6477599dde	\N	\N	{"admin": false, "username": "alice"}	2026-11-02 13:40:15.716499+00
01a19c7d-3d3d-73aa-a68d-07a801c03fb1	01a19c7d-3cdd-740a-ac7a-ee6477599dde	127.0.0.1	login.success	user	01a19c7d-3cdd-740a-ac7a-ee6477599dde	\N	\N	{}	2026-11-02 13:40:15.807124+00
01a19c7d-3da2-709a-bb03-88b17b24bd89	01a19c7d-3cdd-740a-ac7a-ee6477599dde	127.0.0.1	repo.create	repository	01a19c7d-3d56-769e-a5f7-3f3b79610783	01a19c7d-3d56-769e-a5f7-3f3b79610783	\N	{"name": "alice/demo", "visibility": "private"}	2026-11-02 13:40:15.906831+00
01a19c7d-3ddd-70a5-98c9-a8dadada762e	01a19c7d-3cdd-740a-ac7a-ee6477599dde	127.0.0.1	login.success	user	01a19c7d-3cdd-740a-ac7a-ee6477599dde	\N	\N	{}	2026-11-02 13:40:15.966734+00
01a19c7d-3def-72e8-b56e-dad65c69ce0e	01a19c7d-3cdd-740a-ac7a-ee6477599dde	127.0.0.1	token.create	token	01a19c7d-3ded-71c5-ab25-0fba197909af	\N	\N	{"name": "tests", "scopes": ["repo:write", "user:read"], "expires_in_days": 30}	2026-11-02 13:40:15.98467+00
01a19c7d-3efe-76c7-aaa0-03f13f055548	\N	\N	user.create	user	01a19c7d-3ef9-7622-b233-a06d9df83c3e	\N	\N	{"admin": false, "username": "bob"}	2026-11-02 13:40:16.256138+00
01a19c7d-401a-7343-84fa-ec8f08d7d9d9	\N	\N	user.create	user	01a19c7d-4016-705f-93e7-d72c7202775b	\N	\N	{"admin": true, "username": "root-admin"}	2026-11-02 13:40:16.540165+00
01a19c7d-415f-714d-bed2-6b86ecd0c584	01a19c7d-3cdd-740a-ac7a-ee6477599dde	127.0.0.1	login.success	user	01a19c7d-3cdd-740a-ac7a-ee6477599dde	\N	\N	{}	2026-11-02 13:40:16.864208+00
01a19c7d-4178-71ca-89a9-6477dc65d782	01a19c7d-3cdd-740a-ac7a-ee6477599dde	127.0.0.1	org.create	organization	01a19c7d-416f-7156-ab52-622d266fcf0c	\N	01a19c7d-416f-7156-ab52-622d266fcf0c	{"name": "acme"}	2026-11-02 13:40:16.889414+00
01a19c7d-4192-7773-9c6f-f55e4de059ec	01a19c7d-3cdd-740a-ac7a-ee6477599dde	127.0.0.1	org.member.set	organization	01a19c7d-416f-7156-ab52-622d266fcf0c	\N	01a19c7d-416f-7156-ab52-622d266fcf0c	{"role": "member", "username": "bob"}	2026-11-02 13:40:16.915088+00
01a19c7d-41dd-73e7-a264-e74f41dd4853	01a19c7d-3cdd-740a-ac7a-ee6477599dde	127.0.0.1	repo.create	repository	01a19c7d-419d-75cd-9789-fc937b819993	01a19c7d-419d-75cd-9789-fc937b819993	01a19c7d-416f-7156-ab52-622d266fcf0c	{"name": "acme/tools", "visibility": "public"}	2026-11-02 13:40:16.991066+00
01a19c7d-41f8-7501-a91a-3ef79d7d00ae	01a19c7d-3cdd-740a-ac7a-ee6477599dde	127.0.0.1	repo.collaborator.set	repository	01a19c7d-3d56-769e-a5f7-3f3b79610783	01a19c7d-3d56-769e-a5f7-3f3b79610783	\N	{"role": "write", "username": "bob"}	2026-11-02 13:40:17.016905+00
01a19c7d-423c-713b-9f8c-03dfedd17b18	01a19c7d-3cdd-740a-ac7a-ee6477599dde	127.0.0.1	repo.create	repository	01a19c7d-4202-7481-8ec6-566e47cd65c2	01a19c7d-4202-7481-8ec6-566e47cd65c2	\N	{"name": "alice/renamed", "visibility": "private"}	2026-11-02 13:40:17.084653+00
01a19c7d-4250-77a6-b3d1-55e6c94f6b6b	01a19c7d-3cdd-740a-ac7a-ee6477599dde	127.0.0.1	repo.rename	repository	01a19c7d-4202-7481-8ec6-566e47cd65c2	01a19c7d-4202-7481-8ec6-566e47cd65c2	\N	{"to": "new-name", "from": "renamed"}	2026-11-02 13:40:17.104968+00
01a19c7d-42a6-7546-a3d3-4d96ddfbdb52	01a19c7d-3ef9-7622-b233-a06d9df83c3e	127.0.0.1	login.success	user	01a19c7d-3ef9-7622-b233-a06d9df83c3e	\N	\N	{}	2026-11-02 13:40:17.191158+00
01a19c7d-42b7-7261-b4f5-075fd16d9e8a	01a19c7d-3ef9-7622-b233-a06d9df83c3e	127.0.0.1	ssh_key.add	ssh_key	01a19c7d-42b4-76f5-ac4f-c7083eb527fa	\N	\N	{"title": "test key", "fingerprint": "SHA256:w1ReCc+oDMU3oZDfFwiJNdA3kWszEdhbY9OXuQhSE+0"}	2026-11-02 13:40:17.208096+00
01a19c7d-42eb-741e-a15a-b718ba847973	01a19c7d-3ef9-7622-b233-a06d9df83c3e	127.0.0.1	login.success	user	01a19c7d-3ef9-7622-b233-a06d9df83c3e	\N	\N	{}	2026-11-02 13:40:17.259545+00
01a19c7d-42f8-776f-9088-bc42519a71d9	01a19c7d-3ef9-7622-b233-a06d9df83c3e	127.0.0.1	token.create	token	01a19c7d-42f6-72ea-a7eb-6491ffa8bd7b	\N	\N	{"name": "ci", "scopes": ["repo:read"], "expires_in_days": 30}	2026-11-02 13:40:17.273803+00
01a19c7d-4339-75c7-bd5c-f2ccd8f00d37	01a19c7d-3cdd-740a-ac7a-ee6477599dde	127.0.0.1	repo.create	repository	01a19c7d-4307-73cf-abb3-670036c0f11b	01a19c7d-4307-73cf-abb3-670036c0f11b	\N	{"name": "alice/inside", "visibility": "internal"}	2026-11-02 13:40:17.337794+00
01a19c7d-434c-72ae-8ce2-b4cc131d233b	01a19c7d-3cdd-740a-ac7a-ee6477599dde	127.0.0.1	team.create	organization	01a19c7d-416f-7156-ab52-622d266fcf0c	\N	01a19c7d-416f-7156-ab52-622d266fcf0c	{"team": "core"}	2026-11-02 13:40:17.356792+00
01a19c7d-4369-72b1-8710-49f440e2f03b	01a19c7d-3cdd-740a-ac7a-ee6477599dde	127.0.0.1	team.member.add	organization	01a19c7d-416f-7156-ab52-622d266fcf0c	\N	01a19c7d-416f-7156-ab52-622d266fcf0c	{"team": "core", "username": "bob"}	2026-11-02 13:40:17.386236+00
01a19c7d-4388-72fe-baed-d4b94f5dc800	01a19c7d-3cdd-740a-ac7a-ee6477599dde	127.0.0.1	repo.team.set	repository	01a19c7d-419d-75cd-9789-fc937b819993	01a19c7d-419d-75cd-9789-fc937b819993	\N	{"role": "write", "team": "core"}	2026-11-02 13:40:17.417203+00
01a19c7d-43a3-7778-b109-9c6baf6078d9	01a19c7d-3cdd-740a-ac7a-ee6477599dde	127.0.0.1	protection.set	repository	01a19c7d-3d56-769e-a5f7-3f3b79610783	01a19c7d-3d56-769e-a5f7-3f3b79610783	\N	{"pattern": "main", "push_role": "maintain", "require_mr": false, "block_deletion": false, "block_force_push": true}	2026-11-02 13:40:17.444471+00
01a19c7d-43b0-75bd-9028-024e09065f08	01a19c7d-3cdd-740a-ac7a-ee6477599dde	127.0.0.1	token.create	token	01a19c7d-43af-70ab-9175-a75539462f8c	\N	\N	{"name": "fixture", "scopes": ["repo:read", "issues:write"], "expires_in_days": 30}	2026-11-02 13:40:17.457698+00
\.


--
-- Data for Name: background_migrations; Type: TABLE DATA; Schema: public; Owner: slath
--

COPY public.background_migrations (name, introduced_in, state, cursor, processed, total_estimate, last_error, started_at, finished_at, updated_at) FROM stdin;
\.


--
-- Data for Name: blobs; Type: TABLE DATA; Schema: public; Owner: slath
--

COPY public.blobs (sha256, size_bytes, refcount, unreferenced_since, created_at) FROM stdin;
\.


--
-- Data for Name: branch_protections; Type: TABLE DATA; Schema: public; Owner: slath
--

COPY public.branch_protections (id, repo_id, pattern, block_force_push, block_deletion, require_mr, required_approvals, required_status_contexts, push_role, dismiss_stale_approvals, statuses_on_head_ok, require_resolved_threads, bots_count_as_approvers, bot_merge_allowlist, require_up_to_date, created_at) FROM stdin;
01a19c7d-43a1-768e-a6a2-08c4ac743f50	01a19c7d-3d56-769e-a5f7-3f3b79610783	main	t	f	f	1	{}	maintain	t	t	f	f	{}	f	2026-11-02 13:40:17.442483+00
\.


--
-- Data for Name: comments; Type: TABLE DATA; Schema: public; Owner: slath
--

COPY public.comments (id, repo_id, subject_kind, subject_id, author_id, body, edited_at, deleted_at, hidden_at, created_at) FROM stdin;
01a19c7d-44aa-758d-a3ae-a76d92780b9f	01a19c7d-3d56-769e-a5f7-3f3b79610783	issue	01a19c7d-43db-7570-9a02-0cb97e30f9b8	01a19c7d-3ef9-7622-b233-a06d9df83c3e	On it, thanks @alice. See #2.	\N	\N	\N	2026-11-02 13:40:17.707285+00
\.


--
-- Data for Name: email_verifications; Type: TABLE DATA; Schema: public; Owner: slath
--

COPY public.email_verifications (token_hash, user_id, email, expires_at, used_at, created_at) FROM stdin;
\.


--
-- Data for Name: encryption_keys; Type: TABLE DATA; Schema: public; Owner: slath
--

COPY public.encryption_keys (key_id, created_at, retired_at) FROM stdin;
6ed73421	2026-11-02 13:40:15.319188+00	\N
\.


--
-- Data for Name: git_refs; Type: TABLE DATA; Schema: public; Owner: slath
--

COPY public.git_refs (repo_id, name, oid, updated_at) FROM stdin;
01a19c7d-3d56-769e-a5f7-3f3b79610783	\\x726566732f68656164732f6d61696e	e31a2bdcfa4366d63eec483681f2e7bef2528dd9	2026-11-02 13:40:16.783867+00
01a19c7d-3d56-769e-a5f7-3f3b79610783	\\x726566732f746167732f72656c656173652f322e30	822b6c5fab6d985b4dbf8caccf6ca5db811ab916	2026-11-02 13:40:16.783867+00
01a19c7d-3d56-769e-a5f7-3f3b79610783	\\x726566732f746167732f747265652d746167	b952f8c74fe4b4f428492d75c1e227657be2bf48	2026-11-02 13:40:16.783867+00
01a19c7d-3d56-769e-a5f7-3f3b79610783	\\x726566732f746167732f76312e30	032aee9a437815204df8a21bbc7371dcec2bd0e3	2026-11-02 13:40:16.783867+00
01a19c7d-3d56-769e-a5f7-3f3b79610783	\\x726566732f746167732f76312e302d6c69676874	3f3ec41f7a4fefa97602911c0258528cc2ad3d25	2026-11-02 13:40:16.783867+00
01a19c7d-3d56-769e-a5f7-3f3b79610783	\\x726566732f746167732f76322e302d6d657461	e1e0b1fbb57d0584f431150d048088a9e1508dbe	2026-11-02 13:40:16.783867+00
\.


--
-- Data for Name: idempotency_keys; Type: TABLE DATA; Schema: public; Owner: slath
--

COPY public.idempotency_keys (principal_id, key, method, path, request_sha256, status, response, created_at) FROM stdin;
\.


--
-- Data for Name: instance_settings; Type: TABLE DATA; Schema: public; Owner: slath
--

COPY public.instance_settings (key, value, updated_at) FROM stdin;
\.


--
-- Data for Name: invitations; Type: TABLE DATA; Schema: public; Owner: slath
--

COPY public.invitations (id, token_hash, created_by, email, note, expires_at, used_at, used_by, revoked_at, created_at) FROM stdin;
\.


--
-- Data for Name: issue_assignees; Type: TABLE DATA; Schema: public; Owner: slath
--

COPY public.issue_assignees (issue_id, user_id) FROM stdin;
01a19c7d-43db-7570-9a02-0cb97e30f9b8	01a19c7d-3ef9-7622-b233-a06d9df83c3e
\.


--
-- Data for Name: issue_labels; Type: TABLE DATA; Schema: public; Owner: slath
--

COPY public.issue_labels (issue_id, label_id) FROM stdin;
01a19c7d-43db-7570-9a02-0cb97e30f9b8	01a19c7d-3d5f-7637-9bac-2cf4764432bf
01a19c7d-43db-7570-9a02-0cb97e30f9b8	01a19c7d-3d65-7582-824b-452c6bbd1d6a
\.


--
-- Data for Name: issues; Type: TABLE DATA; Schema: public; Owner: slath
--

COPY public.issues (id, repo_id, number, author_id, title, body, state, state_reason, milestone_id, closed_at, created_at, updated_at) FROM stdin;
01a19c7d-4457-7176-9c40-95c06b2cc6fd	01a19c7d-3d56-769e-a5f7-3f3b79610783	2	01a19c7d-3cdd-740a-ac7a-ee6477599dde	Document the config file		closed	completed	\N	2026-11-02 13:40:17.645433+00	2026-11-02 13:40:17.622675+00	2026-11-02 13:40:17.645433+00
01a19c7d-43db-7570-9a02-0cb97e30f9b8	01a19c7d-3d56-769e-a5f7-3f3b79610783	1	01a19c7d-3cdd-740a-ac7a-ee6477599dde	Crash when the config is empty	Steps:\n\n1. Start with an empty file.\n\n@bob, can you look?	open	\N	01a19c7d-43c8-76c0-8069-50a4b3df3f8c	\N	2026-11-02 13:40:17.498434+00	2026-11-02 13:40:17.710753+00
\.


--
-- Data for Name: jobs; Type: TABLE DATA; Schema: public; Owner: slath
--

COPY public.jobs (id, queue, kind, payload, dedupe_key, state, run_at, attempts, max_attempts, locked_by, locked_until, last_error, created_at, finished_at) FROM stdin;
1	default	ref.updated	{"push_id": "01a19c7d-410f-74b2-a4d3-5e06ab13a342", "repo_id": "01a19c7d-3d56-769e-a5f7-3f3b79610783"}	ref.updated:01a19c7d-410f-74b2-a4d3-5e06ab13a342	done	2026-11-02 13:40:16.783867+00	1	8	\N	\N	\N	2026-11-02 13:40:16.783867+00	2026-11-02 13:40:16.800088+00
2	default	notify.fanout	{"repo_id": "01a19c7d-3d56-769e-a5f7-3f3b79610783", "activity": "opened", "actor_id": "01a19c7d-3cdd-740a-ac7a-ee6477599dde", "assigned": [], "issue_id": "01a19c7d-43db-7570-9a02-0cb97e30f9b8", "comment_id": null}	notify.fanout:01a19c7d-43f4-727c-ace9-7e7a560b577c	done	2026-11-02 13:40:17.526051+00	1	8	\N	\N	\N	2026-11-02 13:40:17.526051+00	2026-11-02 13:40:17.562573+00
3	default	notify.fanout	{"repo_id": "01a19c7d-3d56-769e-a5f7-3f3b79610783", "activity": "assigned", "actor_id": "01a19c7d-3cdd-740a-ac7a-ee6477599dde", "assigned": ["01a19c7d-3ef9-7622-b233-a06d9df83c3e"], "issue_id": "01a19c7d-43db-7570-9a02-0cb97e30f9b8", "comment_id": null}	notify.fanout:01a19c7d-442f-7762-bccc-93bde641ffbc	done	2026-11-02 13:40:17.583635+00	1	8	\N	\N	\N	2026-11-02 13:40:17.583635+00	2026-11-02 13:40:17.613595+00
4	default	notify.fanout	{"repo_id": "01a19c7d-3d56-769e-a5f7-3f3b79610783", "activity": "opened", "actor_id": "01a19c7d-3cdd-740a-ac7a-ee6477599dde", "assigned": [], "issue_id": "01a19c7d-4457-7176-9c40-95c06b2cc6fd", "comment_id": null}	notify.fanout:01a19c7d-445a-7106-a1c5-481c8d7042f6	done	2026-11-02 13:40:17.627917+00	1	8	\N	\N	\N	2026-11-02 13:40:17.627917+00	2026-11-02 13:40:17.649704+00
5	default	notify.fanout	{"repo_id": "01a19c7d-3d56-769e-a5f7-3f3b79610783", "activity": "closed", "actor_id": "01a19c7d-3cdd-740a-ac7a-ee6477599dde", "assigned": [], "issue_id": "01a19c7d-4457-7176-9c40-95c06b2cc6fd", "comment_id": null}	notify.fanout:01a19c7d-4471-709f-85e9-1a7507e0862f	done	2026-11-02 13:40:17.649673+00	1	8	\N	\N	\N	2026-11-02 13:40:17.649673+00	2026-11-02 13:40:17.663498+00
6	default	notify.fanout	{"repo_id": "01a19c7d-3d56-769e-a5f7-3f3b79610783", "activity": "commented", "actor_id": "01a19c7d-3ef9-7622-b233-a06d9df83c3e", "assigned": [], "issue_id": "01a19c7d-43db-7570-9a02-0cb97e30f9b8", "comment_id": "01a19c7d-44aa-758d-a3ae-a76d92780b9f"}	notify.fanout:01a19c7d-44bb-71e5-8146-f72b798c12e4	done	2026-11-02 13:40:17.724291+00	1	8	\N	\N	\N	2026-11-02 13:40:17.724291+00	2026-11-02 13:40:17.748538+00
\.


--
-- Data for Name: labels; Type: TABLE DATA; Schema: public; Owner: slath
--

COPY public.labels (id, repo_id, name, color, description, created_at) FROM stdin;
01a19c7d-3d5f-7637-9bac-2cf4764432bf	01a19c7d-3d56-769e-a5f7-3f3b79610783	bug	d73a4a	Something isn't working	2026-11-02 13:40:15.830988+00
01a19c7d-3d62-77ec-8886-4c0b0369c8d2	01a19c7d-3d56-769e-a5f7-3f3b79610783	enhancement	a2eeef	A new feature or an improvement	2026-11-02 13:40:15.830988+00
01a19c7d-3d63-75f2-94af-154d3f2a7360	01a19c7d-3d56-769e-a5f7-3f3b79610783	documentation	0075ca	Changes to the documentation	2026-11-02 13:40:15.830988+00
01a19c7d-3d64-721b-8e85-2dd3110ecdf3	01a19c7d-3d56-769e-a5f7-3f3b79610783	question	d876e3	Needs more information	2026-11-02 13:40:15.830988+00
01a19c7d-3d65-7582-824b-452c6bbd1d6a	01a19c7d-3d56-769e-a5f7-3f3b79610783	good first issue	7057ff	Good for newcomers	2026-11-02 13:40:15.830988+00
01a19c7d-3d65-7582-824b-452d2ed2f78e	01a19c7d-3d56-769e-a5f7-3f3b79610783	help wanted	008672	Extra attention is welcome	2026-11-02 13:40:15.830988+00
01a19c7d-3d66-747d-8731-3f36f8abb75c	01a19c7d-3d56-769e-a5f7-3f3b79610783	duplicate	cfd3d7	This issue already exists	2026-11-02 13:40:15.830988+00
01a19c7d-3d67-717e-975a-15ad0591f3d3	01a19c7d-3d56-769e-a5f7-3f3b79610783	wontfix	ffffff	This will not be worked on	2026-11-02 13:40:15.830988+00
01a19c7d-41a4-704c-a0d4-8d7e4577881a	01a19c7d-419d-75cd-9789-fc937b819993	bug	d73a4a	Something isn't working	2026-11-02 13:40:16.925995+00
01a19c7d-41a6-72f9-99fd-cc1314b95a6e	01a19c7d-419d-75cd-9789-fc937b819993	enhancement	a2eeef	A new feature or an improvement	2026-11-02 13:40:16.925995+00
01a19c7d-41a7-757e-81c6-a0f7e404ccc6	01a19c7d-419d-75cd-9789-fc937b819993	documentation	0075ca	Changes to the documentation	2026-11-02 13:40:16.925995+00
01a19c7d-41a8-722b-8fd0-9e28943c170d	01a19c7d-419d-75cd-9789-fc937b819993	question	d876e3	Needs more information	2026-11-02 13:40:16.925995+00
01a19c7d-41a9-702a-9e09-4d9ef9d92799	01a19c7d-419d-75cd-9789-fc937b819993	good first issue	7057ff	Good for newcomers	2026-11-02 13:40:16.925995+00
01a19c7d-41a9-702a-9e09-4d9f55b66d1c	01a19c7d-419d-75cd-9789-fc937b819993	help wanted	008672	Extra attention is welcome	2026-11-02 13:40:16.925995+00
01a19c7d-41aa-7138-b46d-125c83c96bb1	01a19c7d-419d-75cd-9789-fc937b819993	duplicate	cfd3d7	This issue already exists	2026-11-02 13:40:16.925995+00
01a19c7d-41aa-7138-b46d-125d4ece09cd	01a19c7d-419d-75cd-9789-fc937b819993	wontfix	ffffff	This will not be worked on	2026-11-02 13:40:16.925995+00
01a19c7d-4204-7023-b4d0-d45f0d9f17ed	01a19c7d-4202-7481-8ec6-566e47cd65c2	bug	d73a4a	Something isn't working	2026-11-02 13:40:17.027028+00
01a19c7d-4205-7152-8690-5ce0a4ad46cf	01a19c7d-4202-7481-8ec6-566e47cd65c2	enhancement	a2eeef	A new feature or an improvement	2026-11-02 13:40:17.027028+00
01a19c7d-4205-7152-8690-5ce1b2441a17	01a19c7d-4202-7481-8ec6-566e47cd65c2	documentation	0075ca	Changes to the documentation	2026-11-02 13:40:17.027028+00
01a19c7d-4206-7728-b2c9-f311d2e790d7	01a19c7d-4202-7481-8ec6-566e47cd65c2	question	d876e3	Needs more information	2026-11-02 13:40:17.027028+00
01a19c7d-4206-7728-b2c9-f31262db9863	01a19c7d-4202-7481-8ec6-566e47cd65c2	good first issue	7057ff	Good for newcomers	2026-11-02 13:40:17.027028+00
01a19c7d-4207-760f-9fc2-1f4543a88e88	01a19c7d-4202-7481-8ec6-566e47cd65c2	help wanted	008672	Extra attention is welcome	2026-11-02 13:40:17.027028+00
01a19c7d-4207-760f-9fc2-1f46ecfd3c9a	01a19c7d-4202-7481-8ec6-566e47cd65c2	duplicate	cfd3d7	This issue already exists	2026-11-02 13:40:17.027028+00
01a19c7d-4208-745f-b061-a4d28278a6e2	01a19c7d-4202-7481-8ec6-566e47cd65c2	wontfix	ffffff	This will not be worked on	2026-11-02 13:40:17.027028+00
01a19c7d-4309-717d-830b-81541a2e8c90	01a19c7d-4307-73cf-abb3-670036c0f11b	bug	d73a4a	Something isn't working	2026-11-02 13:40:17.28744+00
01a19c7d-4309-717d-830b-8155442a10b8	01a19c7d-4307-73cf-abb3-670036c0f11b	enhancement	a2eeef	A new feature or an improvement	2026-11-02 13:40:17.28744+00
01a19c7d-430a-7583-85f6-732d720044a4	01a19c7d-4307-73cf-abb3-670036c0f11b	documentation	0075ca	Changes to the documentation	2026-11-02 13:40:17.28744+00
01a19c7d-430a-7583-85f6-732e36811397	01a19c7d-4307-73cf-abb3-670036c0f11b	question	d876e3	Needs more information	2026-11-02 13:40:17.28744+00
01a19c7d-430a-7583-85f6-732f1a583b38	01a19c7d-4307-73cf-abb3-670036c0f11b	good first issue	7057ff	Good for newcomers	2026-11-02 13:40:17.28744+00
01a19c7d-430b-7671-948c-6231133283b0	01a19c7d-4307-73cf-abb3-670036c0f11b	help wanted	008672	Extra attention is welcome	2026-11-02 13:40:17.28744+00
01a19c7d-430c-7001-9e9d-a96c8d2b7df3	01a19c7d-4307-73cf-abb3-670036c0f11b	duplicate	cfd3d7	This issue already exists	2026-11-02 13:40:17.28744+00
01a19c7d-430d-7587-b9b5-f906150462d3	01a19c7d-4307-73cf-abb3-670036c0f11b	wontfix	ffffff	This will not be worked on	2026-11-02 13:40:17.28744+00
\.


--
-- Data for Name: login_attempts; Type: TABLE DATA; Schema: public; Owner: slath
--

COPY public.login_attempts (key, window_start, count) FROM stdin;
\.


--
-- Data for Name: login_challenges; Type: TABLE DATA; Schema: public; Owner: slath
--

COPY public.login_challenges (id_hash, user_id, purpose, first_factor, data, failures, expires_at, created_at) FROM stdin;
\.


--
-- Data for Name: milestones; Type: TABLE DATA; Schema: public; Owner: slath
--

COPY public.milestones (id, repo_id, title, description, due_on, state, created_at, updated_at) FROM stdin;
01a19c7d-43c8-76c0-8069-50a4b3df3f8c	01a19c7d-3d56-769e-a5f7-3f3b79610783	v1.0	\N	2027-01-31	open	2026-11-02 13:40:17.482013+00	2026-11-02 13:40:17.482013+00
\.


--
-- Data for Name: namespaces; Type: TABLE DATA; Schema: public; Owner: slath
--

COPY public.namespaces (id, name, kind, created_at) FROM stdin;
01a19c7d-3cdd-740a-ac7a-ee6302ae805c	alice	user	2026-11-02 13:40:15.70665+00
01a19c7d-3ef9-7622-b233-a06c34e40187	bob	user	2026-11-02 13:40:16.246859+00
01a19c7d-4016-705f-93e7-d72be50ea597	root-admin	user	2026-11-02 13:40:16.530898+00
01a19c7d-416f-7156-ab52-622ca4050197	acme	org	2026-11-02 13:40:16.877244+00
\.


--
-- Data for Name: notifications; Type: TABLE DATA; Schema: public; Owner: slath
--

COPY public.notifications (id, user_id, repo_id, subject_kind, subject_id, reason, unread, updated_at) FROM stdin;
01a19c7d-4416-75b0-bf70-9e678dd8abe2	01a19c7d-3ef9-7622-b233-a06d9df83c3e	01a19c7d-3d56-769e-a5f7-3f3b79610783	issue	01a19c7d-43db-7570-9a02-0cb97e30f9b8	assigned	f	2026-11-02 13:40:17.612029+00
01a19c7d-44d2-7723-b89c-1fc6c67373f9	01a19c7d-3cdd-740a-ac7a-ee6477599dde	01a19c7d-3d56-769e-a5f7-3f3b79610783	issue	01a19c7d-43db-7570-9a02-0cb97e30f9b8	mentioned	t	2026-11-02 13:40:17.746876+00
\.


--
-- Data for Name: oidc_identities; Type: TABLE DATA; Schema: public; Owner: slath
--

COPY public.oidc_identities (issuer, subject, user_id, provider_key, email, created_at, last_login_at) FROM stdin;
\.


--
-- Data for Name: oidc_pending; Type: TABLE DATA; Schema: public; Owner: slath
--

COPY public.oidc_pending (token_hash, provider_key, browser_hash, issuer, subject, email, email_verified, preferred_username, display_name, expires_at) FROM stdin;
\.


--
-- Data for Name: oidc_states; Type: TABLE DATA; Schema: public; Owner: slath
--

COPY public.oidc_states (state_hash, provider_key, browser_hash, nonce, pkce_verifier, purpose, user_id, next, expires_at) FROM stdin;
\.


--
-- Data for Name: org_members; Type: TABLE DATA; Schema: public; Owner: slath
--

COPY public.org_members (org_id, user_id, role, created_at) FROM stdin;
01a19c7d-416f-7156-ab52-622d266fcf0c	01a19c7d-3cdd-740a-ac7a-ee6477599dde	owner	2026-11-02 13:40:16.877244+00
01a19c7d-416f-7156-ab52-622d266fcf0c	01a19c7d-3ef9-7622-b233-a06d9df83c3e	member	2026-11-02 13:40:16.912558+00
\.


--
-- Data for Name: organizations; Type: TABLE DATA; Schema: public; Owner: slath
--

COPY public.organizations (id, namespace_id, display_name, description, default_member_role, created_at, updated_at, require_mfa, avatar_blob_sha256) FROM stdin;
01a19c7d-416f-7156-ab52-622d266fcf0c	01a19c7d-416f-7156-ab52-622ca4050197	Acme	\N	none	2026-11-02 13:40:16.877244+00	2026-11-02 13:40:16.877244+00	f	\N
\.


--
-- Data for Name: password_resets; Type: TABLE DATA; Schema: public; Owner: slath
--

COPY public.password_resets (token_hash, user_id, expires_at, used_at, created_at) FROM stdin;
\.


--
-- Data for Name: push_events; Type: TABLE DATA; Schema: public; Owner: slath
--

COPY public.push_events (id, repo_id, pusher_id, transport, created_at) FROM stdin;
01a19c7d-410f-74b2-a4d3-5e06ab13a342	01a19c7d-3d56-769e-a5f7-3f3b79610783	01a19c7d-3cdd-740a-ac7a-ee6477599dde	http	2026-11-02 13:40:16.783867+00
\.


--
-- Data for Name: recovery_codes; Type: TABLE DATA; Schema: public; Owner: slath
--

COPY public.recovery_codes (id, user_id, code_hash, used_at, created_at) FROM stdin;
\.


--
-- Data for Name: ref_updates; Type: TABLE DATA; Schema: public; Owner: slath
--

COPY public.ref_updates (push_id, seq, name, old_oid, new_oid) FROM stdin;
01a19c7d-410f-74b2-a4d3-5e06ab13a342	0	\\x726566732f68656164732f6d61696e	\N	e31a2bdcfa4366d63eec483681f2e7bef2528dd9
01a19c7d-410f-74b2-a4d3-5e06ab13a342	1	\\x726566732f746167732f72656c656173652f322e30	\N	822b6c5fab6d985b4dbf8caccf6ca5db811ab916
01a19c7d-410f-74b2-a4d3-5e06ab13a342	2	\\x726566732f746167732f747265652d746167	\N	b952f8c74fe4b4f428492d75c1e227657be2bf48
01a19c7d-410f-74b2-a4d3-5e06ab13a342	3	\\x726566732f746167732f76312e30	\N	032aee9a437815204df8a21bbc7371dcec2bd0e3
01a19c7d-410f-74b2-a4d3-5e06ab13a342	4	\\x726566732f746167732f76312e302d6c69676874	\N	3f3ec41f7a4fefa97602911c0258528cc2ad3d25
01a19c7d-410f-74b2-a4d3-5e06ab13a342	5	\\x726566732f746167732f76322e302d6d657461	\N	e1e0b1fbb57d0584f431150d048088a9e1508dbe
\.


--
-- Data for Name: repo_collaborators; Type: TABLE DATA; Schema: public; Owner: slath
--

COPY public.repo_collaborators (repo_id, user_id, role, created_at) FROM stdin;
01a19c7d-419d-75cd-9789-fc937b819993	01a19c7d-3cdd-740a-ac7a-ee6477599dde	admin	2026-11-02 13:40:16.925995+00
01a19c7d-3d56-769e-a5f7-3f3b79610783	01a19c7d-3ef9-7622-b233-a06d9df83c3e	write	2026-11-02 13:40:17.014766+00
\.


--
-- Data for Name: repo_imports; Type: TABLE DATA; Schema: public; Owner: slath
--

COPY public.repo_imports (repo_id, url, requested_by, state, error, branches, tags, size_bytes, created_at, started_at, finished_at) FROM stdin;
\.


--
-- Data for Name: repo_redirects; Type: TABLE DATA; Schema: public; Owner: slath
--

COPY public.repo_redirects (namespace_id, name, repo_id, created_at) FROM stdin;
01a19c7d-3cdd-740a-ac7a-ee6302ae805c	renamed	01a19c7d-4202-7481-8ec6-566e47cd65c2	2026-11-02 13:40:17.098569+00
\.


--
-- Data for Name: repo_team_access; Type: TABLE DATA; Schema: public; Owner: slath
--

COPY public.repo_team_access (repo_id, team_id, role) FROM stdin;
01a19c7d-419d-75cd-9789-fc937b819993	01a19c7d-4349-7153-bcb1-e94e7554b3d7	write
\.


--
-- Data for Name: repositories; Type: TABLE DATA; Schema: public; Owner: slath
--

COPY public.repositories (id, namespace_id, name, description, visibility, default_branch, state, fork_parent_id, fork_network_id, allow_forking, trust_bot_runs, auto_approve_external_for, issues_enabled, merge_requests_enabled, size_bytes, next_item_number, merge_strategies, default_merge_strategy, delete_source_branch_default, archived_reason, deleted_at, pushed_at, created_at, updated_at, pushes_since_maintenance, maintained_at, maintenance_runs, maintenance_lease_until) FROM stdin;
01a19c7d-419d-75cd-9789-fc937b819993	01a19c7d-416f-7156-ab52-622ca4050197	tools	\N	public	main	active	\N	01a19c7d-419d-75cd-9789-fc937b819993	t	f	none	t	t	0	1	{fast-forward,rebase,merge-commit,squash}	rebase	t	\N	\N	\N	2026-11-02 13:40:16.925995+00	2026-11-02 13:40:16.925995+00	0	\N	0	\N
01a19c7d-4202-7481-8ec6-566e47cd65c2	01a19c7d-3cdd-740a-ac7a-ee6302ae805c	new-name	\N	private	main	active	\N	01a19c7d-4202-7481-8ec6-566e47cd65c2	t	f	none	t	t	0	1	{fast-forward,rebase,merge-commit,squash}	rebase	t	\N	\N	\N	2026-11-02 13:40:17.027028+00	2026-11-02 13:40:17.098569+00	0	\N	0	\N
01a19c7d-4307-73cf-abb3-670036c0f11b	01a19c7d-3cdd-740a-ac7a-ee6302ae805c	inside	\N	internal	main	active	\N	01a19c7d-4307-73cf-abb3-670036c0f11b	t	f	none	t	t	0	1	{fast-forward,rebase,merge-commit,squash}	rebase	t	\N	\N	\N	2026-11-02 13:40:17.28744+00	2026-11-02 13:40:17.28744+00	0	\N	0	\N
01a19c7d-3d56-769e-a5f7-3f3b79610783	01a19c7d-3cdd-740a-ac7a-ee6302ae805c	demo	\N	private	main	active	\N	01a19c7d-3d56-769e-a5f7-3f3b79610783	t	f	none	t	t	40960	3	{fast-forward,rebase,merge-commit,squash}	rebase	t	\N	\N	2026-11-02 13:40:16.783867+00	2026-11-02 13:40:15.830988+00	2026-11-02 13:40:15.830988+00	1	\N	0	\N
\.


--
-- Data for Name: reserved_names; Type: TABLE DATA; Schema: public; Owner: slath
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
-- Data for Name: schema_info; Type: TABLE DATA; Schema: public; Owner: slath
--

COPY public.schema_info (version, singleton) FROM stdin;
100	t
\.


--
-- Data for Name: sessions; Type: TABLE DATA; Schema: public; Owner: slath
--

COPY public.sessions (id_hash, user_id, created_at, last_seen_at, expires_at, auth_level, sudo_until, ip, user_agent) FROM stdin;
\\x57c061d2e7c59f23f2a9f4f1f8c44bee562f1d3af8178d2872a0172ff1cbde0a	01a19c7d-3cdd-740a-ac7a-ee6477599dde	2026-11-02 13:40:15.802345+00	2026-11-02 13:40:15.802345+00	2026-12-02 13:40:15.800676+00	password	2026-11-02 13:55:15.800676+00	127.0.0.1	ureq/3.5.0
\\x97538f46b931defe9196af5168437c62df2df95e00bbb54dd7533ce2aabdf5d3	01a19c7d-3cdd-740a-ac7a-ee6477599dde	2026-11-02 13:40:15.964937+00	2026-11-02 13:40:15.964937+00	2026-12-02 13:40:15.964067+00	password	2026-11-02 13:55:15.964067+00	127.0.0.1	ureq/3.5.0
\\x30d4d0fa41121798589ac0ab6fd2ecb8e3ddff320cb22bd3f7396e78e6b9f135	01a19c7d-3cdd-740a-ac7a-ee6477599dde	2026-11-02 13:40:16.863026+00	2026-11-02 13:40:16.863026+00	2026-12-02 13:40:16.86233+00	password	2026-11-02 13:55:16.86233+00	127.0.0.1	ureq/3.5.0
\\x4c87cf35c828c57d135b9fa1de567bd5a031a6065323f48a738643e7a7f06151	01a19c7d-3ef9-7622-b233-a06d9df83c3e	2026-11-02 13:40:17.189004+00	2026-11-02 13:40:17.189004+00	2026-12-02 13:40:17.18732+00	password	2026-11-02 13:55:17.18732+00	127.0.0.1	ureq/3.5.0
\\x4bd0212f61a6c3d59573c17ebe13f2e99da2736eb63130bee96cfdcf5573e146	01a19c7d-3ef9-7622-b233-a06d9df83c3e	2026-11-02 13:40:17.258343+00	2026-11-02 13:40:17.258343+00	2026-12-02 13:40:17.257069+00	password	2026-11-02 13:55:17.257069+00	127.0.0.1	ureq/3.5.0
\.


--
-- Data for Name: ssh_keys; Type: TABLE DATA; Schema: public; Owner: slath
--

COPY public.ssh_keys (id, user_id, title, key_type, public_key, fingerprint_sha256, last_used_at, created_at) FROM stdin;
01a19c7d-42b4-76f5-ac4f-c7083eb527fa	01a19c7d-3ef9-7622-b233-a06d9df83c3e	test key	ssh-ed25519	AAAAC3NzaC1lZDI1NTE5AAAAIOVmYD2rXs3tPIT4Xq0A2fWp9qc7+5OlagZfrwmYeHEx	SHA256:w1ReCc+oDMU3oZDfFwiJNdA3kWszEdhbY9OXuQhSE+0	\N	2026-11-02 13:40:17.206161+00
\.


--
-- Data for Name: subscriptions; Type: TABLE DATA; Schema: public; Owner: slath
--

COPY public.subscriptions (user_id, subject_kind, subject_id, repo_id, state, created_at) FROM stdin;
01a19c7d-3cdd-740a-ac7a-ee6477599dde	repo	01a19c7d-3d56-769e-a5f7-3f3b79610783	01a19c7d-3d56-769e-a5f7-3f3b79610783	watching	2026-11-02 13:40:15.830988+00
01a19c7d-3cdd-740a-ac7a-ee6477599dde	repo	01a19c7d-419d-75cd-9789-fc937b819993	01a19c7d-419d-75cd-9789-fc937b819993	watching	2026-11-02 13:40:16.925995+00
01a19c7d-3cdd-740a-ac7a-ee6477599dde	repo	01a19c7d-4202-7481-8ec6-566e47cd65c2	01a19c7d-4202-7481-8ec6-566e47cd65c2	watching	2026-11-02 13:40:17.027028+00
01a19c7d-3cdd-740a-ac7a-ee6477599dde	repo	01a19c7d-4307-73cf-abb3-670036c0f11b	01a19c7d-4307-73cf-abb3-670036c0f11b	watching	2026-11-02 13:40:17.28744+00
\.


--
-- Data for Name: team_members; Type: TABLE DATA; Schema: public; Owner: slath
--

COPY public.team_members (team_id, user_id) FROM stdin;
01a19c7d-4349-7153-bcb1-e94e7554b3d7	01a19c7d-3ef9-7622-b233-a06d9df83c3e
\.


--
-- Data for Name: teams; Type: TABLE DATA; Schema: public; Owner: slath
--

COPY public.teams (id, org_id, name, description, created_at) FROM stdin;
01a19c7d-4349-7153-bcb1-e94e7554b3d7	01a19c7d-416f-7156-ab52-622d266fcf0c	core	Core team	2026-11-02 13:40:17.354884+00
\.


--
-- Data for Name: timeline_events; Type: TABLE DATA; Schema: public; Owner: slath
--

COPY public.timeline_events (id, repo_id, subject_kind, subject_id, actor_id, kind, data, created_at) FROM stdin;
01a19c7d-4412-771a-8218-b6d16241ed14	01a19c7d-3d56-769e-a5f7-3f3b79610783	issue	01a19c7d-43db-7570-9a02-0cb97e30f9b8	01a19c7d-3cdd-740a-ac7a-ee6477599dde	labeled	{"label": "bug"}	2026-11-02 13:40:17.55601+00
01a19c7d-4418-7697-8b03-49e8d0ca2df5	01a19c7d-3d56-769e-a5f7-3f3b79610783	issue	01a19c7d-43db-7570-9a02-0cb97e30f9b8	01a19c7d-3cdd-740a-ac7a-ee6477599dde	labeled	{"label": "good first issue"}	2026-11-02 13:40:17.560788+00
01a19c7d-442b-76f4-9f29-853e903ddebb	01a19c7d-3d56-769e-a5f7-3f3b79610783	issue	01a19c7d-43db-7570-9a02-0cb97e30f9b8	01a19c7d-3cdd-740a-ac7a-ee6477599dde	assigned	{"user": "bob"}	2026-11-02 13:40:17.580907+00
01a19c7d-443e-75ed-991c-a86248c461fb	01a19c7d-3d56-769e-a5f7-3f3b79610783	issue	01a19c7d-43db-7570-9a02-0cb97e30f9b8	01a19c7d-3cdd-740a-ac7a-ee6477599dde	milestoned	{"milestone": "v1.0"}	2026-11-02 13:40:17.599032+00
01a19c7d-446f-76c7-bf89-18a48ecbdc7d	01a19c7d-3d56-769e-a5f7-3f3b79610783	issue	01a19c7d-4457-7176-9c40-95c06b2cc6fd	01a19c7d-3cdd-740a-ac7a-ee6477599dde	closed	{"reason": "completed"}	2026-11-02 13:40:17.645433+00
01a19c7d-44b7-7394-a734-581d4c65e654	01a19c7d-3d56-769e-a5f7-3f3b79610783	issue	01a19c7d-4457-7176-9c40-95c06b2cc6fd	01a19c7d-3ef9-7622-b233-a06d9df83c3e	referenced	{"number": 1, "repo_id": "01a19c7d-3d56-769e-a5f7-3f3b79610783"}	2026-11-02 13:40:17.721499+00
\.


--
-- Data for Name: totp_credentials; Type: TABLE DATA; Schema: public; Owner: slath
--

COPY public.totp_credentials (user_id, secret_enc, key_id, last_used_step, confirmed, created_at) FROM stdin;
\.


--
-- Data for Name: upgrade_history; Type: TABLE DATA; Schema: public; Owner: slath
--

COPY public.upgrade_history (id, from_version, to_version, from_schema, to_schema, backup_path, preflight, started_at, finished_at, outcome, error) FROM stdin;
\.


--
-- Data for Name: users; Type: TABLE DATA; Schema: public; Owner: slath
--

COPY public.users (id, namespace_id, kind, bot_owner_kind, bot_owner_id, display_name, email, email_verified, password_hash, is_admin, state, mfa_required, avatar_blob_sha256, timezone, theme, diff_view, editor_mode, created_at, updated_at, density, code_font_size, email_notifications) FROM stdin;
01a19c7d-3cdd-740a-ac7a-ee6477599dde	01a19c7d-3cdd-740a-ac7a-ee6302ae805c	human	\N	\N	\N	alice@example.org	f	$argon2id$v=19$m=19456,t=2,p=1$H+HJDyQX3KW4yAuhkEy+cg$LSq4menhveEBKGjEo0v2TR2p39/VAwuZKe1CnCW2o3g	f	active	f	\N	\N	system	unified	wysiwyg	2026-11-02 13:40:15.70665+00	2026-11-02 13:40:15.70665+00	comfortable	14	t
01a19c7d-3ef9-7622-b233-a06d9df83c3e	01a19c7d-3ef9-7622-b233-a06c34e40187	human	\N	\N	\N	bob@example.org	f	$argon2id$v=19$m=19456,t=2,p=1$Tk5Di/1LVF9XRwYb2H16eA$W5uJVutmNXXPjPBlKJACopxG72z4Jc87viNgTdkO6d8	f	active	f	\N	\N	system	unified	wysiwyg	2026-11-02 13:40:16.246859+00	2026-11-02 13:40:16.246859+00	comfortable	14	t
01a19c7d-4016-705f-93e7-d72c7202775b	01a19c7d-4016-705f-93e7-d72be50ea597	human	\N	\N	\N	root-admin@example.org	f	$argon2id$v=19$m=19456,t=2,p=1$E83mQLqheYe0oMHs69yRyQ$f5FBh6Eu4ITO0gJYTobEGIPkeLG8i/4xcff2tIPrpiQ	t	active	f	\N	\N	system	unified	wysiwyg	2026-11-02 13:40:16.530898+00	2026-11-02 13:40:16.530898+00	comfortable	14	t
\.


--
-- Data for Name: webauthn_ceremonies; Type: TABLE DATA; Schema: public; Owner: slath
--

COPY public.webauthn_ceremonies (id_hash, user_id, kind, state, expires_at, created_at) FROM stdin;
\.


--
-- Data for Name: webauthn_credentials; Type: TABLE DATA; Schema: public; Owner: slath
--

COPY public.webauthn_credentials (id, user_id, credential_id, passkey, name, last_used_at, created_at) FROM stdin;
\.


--
-- Name: jobs_id_seq; Type: SEQUENCE SET; Schema: public; Owner: slath
--

SELECT pg_catalog.setval('public.jobs_id_seq', 6, true);


--
-- Name: _sqlx_migrations _sqlx_migrations_pkey; Type: CONSTRAINT; Schema: public; Owner: slath
--

ALTER TABLE ONLY public._sqlx_migrations
    ADD CONSTRAINT _sqlx_migrations_pkey PRIMARY KEY (version);


--
-- Name: access_tokens access_tokens_pkey; Type: CONSTRAINT; Schema: public; Owner: slath
--

ALTER TABLE ONLY public.access_tokens
    ADD CONSTRAINT access_tokens_pkey PRIMARY KEY (id);


--
-- Name: access_tokens access_tokens_token_hash_key; Type: CONSTRAINT; Schema: public; Owner: slath
--

ALTER TABLE ONLY public.access_tokens
    ADD CONSTRAINT access_tokens_token_hash_key UNIQUE (token_hash);


--
-- Name: attachments attachments_pkey; Type: CONSTRAINT; Schema: public; Owner: slath
--

ALTER TABLE ONLY public.attachments
    ADD CONSTRAINT attachments_pkey PRIMARY KEY (id);


--
-- Name: audit_events audit_events_pkey; Type: CONSTRAINT; Schema: public; Owner: slath
--

ALTER TABLE ONLY public.audit_events
    ADD CONSTRAINT audit_events_pkey PRIMARY KEY (id);


--
-- Name: background_migrations background_migrations_pkey; Type: CONSTRAINT; Schema: public; Owner: slath
--

ALTER TABLE ONLY public.background_migrations
    ADD CONSTRAINT background_migrations_pkey PRIMARY KEY (name);


--
-- Name: blobs blobs_pkey; Type: CONSTRAINT; Schema: public; Owner: slath
--

ALTER TABLE ONLY public.blobs
    ADD CONSTRAINT blobs_pkey PRIMARY KEY (sha256);


--
-- Name: branch_protections branch_protections_pkey; Type: CONSTRAINT; Schema: public; Owner: slath
--

ALTER TABLE ONLY public.branch_protections
    ADD CONSTRAINT branch_protections_pkey PRIMARY KEY (id);


--
-- Name: branch_protections branch_protections_repo_id_pattern_key; Type: CONSTRAINT; Schema: public; Owner: slath
--

ALTER TABLE ONLY public.branch_protections
    ADD CONSTRAINT branch_protections_repo_id_pattern_key UNIQUE (repo_id, pattern);


--
-- Name: comments comments_pkey; Type: CONSTRAINT; Schema: public; Owner: slath
--

ALTER TABLE ONLY public.comments
    ADD CONSTRAINT comments_pkey PRIMARY KEY (id);


--
-- Name: email_verifications email_verifications_pkey; Type: CONSTRAINT; Schema: public; Owner: slath
--

ALTER TABLE ONLY public.email_verifications
    ADD CONSTRAINT email_verifications_pkey PRIMARY KEY (token_hash);


--
-- Name: encryption_keys encryption_keys_pkey; Type: CONSTRAINT; Schema: public; Owner: slath
--

ALTER TABLE ONLY public.encryption_keys
    ADD CONSTRAINT encryption_keys_pkey PRIMARY KEY (key_id);


--
-- Name: git_refs git_refs_pkey; Type: CONSTRAINT; Schema: public; Owner: slath
--

ALTER TABLE ONLY public.git_refs
    ADD CONSTRAINT git_refs_pkey PRIMARY KEY (repo_id, name);


--
-- Name: idempotency_keys idempotency_keys_pkey; Type: CONSTRAINT; Schema: public; Owner: slath
--

ALTER TABLE ONLY public.idempotency_keys
    ADD CONSTRAINT idempotency_keys_pkey PRIMARY KEY (principal_id, key);


--
-- Name: instance_settings instance_settings_pkey; Type: CONSTRAINT; Schema: public; Owner: slath
--

ALTER TABLE ONLY public.instance_settings
    ADD CONSTRAINT instance_settings_pkey PRIMARY KEY (key);


--
-- Name: invitations invitations_pkey; Type: CONSTRAINT; Schema: public; Owner: slath
--

ALTER TABLE ONLY public.invitations
    ADD CONSTRAINT invitations_pkey PRIMARY KEY (id);


--
-- Name: invitations invitations_token_hash_key; Type: CONSTRAINT; Schema: public; Owner: slath
--

ALTER TABLE ONLY public.invitations
    ADD CONSTRAINT invitations_token_hash_key UNIQUE (token_hash);


--
-- Name: issue_assignees issue_assignees_pkey; Type: CONSTRAINT; Schema: public; Owner: slath
--

ALTER TABLE ONLY public.issue_assignees
    ADD CONSTRAINT issue_assignees_pkey PRIMARY KEY (issue_id, user_id);


--
-- Name: issue_labels issue_labels_pkey; Type: CONSTRAINT; Schema: public; Owner: slath
--

ALTER TABLE ONLY public.issue_labels
    ADD CONSTRAINT issue_labels_pkey PRIMARY KEY (issue_id, label_id);


--
-- Name: issues issues_pkey; Type: CONSTRAINT; Schema: public; Owner: slath
--

ALTER TABLE ONLY public.issues
    ADD CONSTRAINT issues_pkey PRIMARY KEY (id);


--
-- Name: issues issues_repo_id_number_key; Type: CONSTRAINT; Schema: public; Owner: slath
--

ALTER TABLE ONLY public.issues
    ADD CONSTRAINT issues_repo_id_number_key UNIQUE (repo_id, number);


--
-- Name: jobs jobs_dedupe_key_key; Type: CONSTRAINT; Schema: public; Owner: slath
--

ALTER TABLE ONLY public.jobs
    ADD CONSTRAINT jobs_dedupe_key_key UNIQUE (dedupe_key);


--
-- Name: jobs jobs_pkey; Type: CONSTRAINT; Schema: public; Owner: slath
--

ALTER TABLE ONLY public.jobs
    ADD CONSTRAINT jobs_pkey PRIMARY KEY (id);


--
-- Name: labels labels_pkey; Type: CONSTRAINT; Schema: public; Owner: slath
--

ALTER TABLE ONLY public.labels
    ADD CONSTRAINT labels_pkey PRIMARY KEY (id);


--
-- Name: labels labels_repo_id_name_key; Type: CONSTRAINT; Schema: public; Owner: slath
--

ALTER TABLE ONLY public.labels
    ADD CONSTRAINT labels_repo_id_name_key UNIQUE (repo_id, name);


--
-- Name: login_attempts login_attempts_pkey; Type: CONSTRAINT; Schema: public; Owner: slath
--

ALTER TABLE ONLY public.login_attempts
    ADD CONSTRAINT login_attempts_pkey PRIMARY KEY (key, window_start);


--
-- Name: login_challenges login_challenges_pkey; Type: CONSTRAINT; Schema: public; Owner: slath
--

ALTER TABLE ONLY public.login_challenges
    ADD CONSTRAINT login_challenges_pkey PRIMARY KEY (id_hash);


--
-- Name: milestones milestones_pkey; Type: CONSTRAINT; Schema: public; Owner: slath
--

ALTER TABLE ONLY public.milestones
    ADD CONSTRAINT milestones_pkey PRIMARY KEY (id);


--
-- Name: milestones milestones_repo_id_title_key; Type: CONSTRAINT; Schema: public; Owner: slath
--

ALTER TABLE ONLY public.milestones
    ADD CONSTRAINT milestones_repo_id_title_key UNIQUE (repo_id, title);


--
-- Name: namespaces namespaces_name_key; Type: CONSTRAINT; Schema: public; Owner: slath
--

ALTER TABLE ONLY public.namespaces
    ADD CONSTRAINT namespaces_name_key UNIQUE (name);


--
-- Name: namespaces namespaces_pkey; Type: CONSTRAINT; Schema: public; Owner: slath
--

ALTER TABLE ONLY public.namespaces
    ADD CONSTRAINT namespaces_pkey PRIMARY KEY (id);


--
-- Name: notifications notifications_pkey; Type: CONSTRAINT; Schema: public; Owner: slath
--

ALTER TABLE ONLY public.notifications
    ADD CONSTRAINT notifications_pkey PRIMARY KEY (id);


--
-- Name: notifications notifications_user_id_subject_kind_subject_id_key; Type: CONSTRAINT; Schema: public; Owner: slath
--

ALTER TABLE ONLY public.notifications
    ADD CONSTRAINT notifications_user_id_subject_kind_subject_id_key UNIQUE (user_id, subject_kind, subject_id);


--
-- Name: oidc_identities oidc_identities_pkey; Type: CONSTRAINT; Schema: public; Owner: slath
--

ALTER TABLE ONLY public.oidc_identities
    ADD CONSTRAINT oidc_identities_pkey PRIMARY KEY (issuer, subject);


--
-- Name: oidc_pending oidc_pending_pkey; Type: CONSTRAINT; Schema: public; Owner: slath
--

ALTER TABLE ONLY public.oidc_pending
    ADD CONSTRAINT oidc_pending_pkey PRIMARY KEY (token_hash);


--
-- Name: oidc_states oidc_states_pkey; Type: CONSTRAINT; Schema: public; Owner: slath
--

ALTER TABLE ONLY public.oidc_states
    ADD CONSTRAINT oidc_states_pkey PRIMARY KEY (state_hash);


--
-- Name: org_members org_members_pkey; Type: CONSTRAINT; Schema: public; Owner: slath
--

ALTER TABLE ONLY public.org_members
    ADD CONSTRAINT org_members_pkey PRIMARY KEY (org_id, user_id);


--
-- Name: organizations organizations_namespace_id_key; Type: CONSTRAINT; Schema: public; Owner: slath
--

ALTER TABLE ONLY public.organizations
    ADD CONSTRAINT organizations_namespace_id_key UNIQUE (namespace_id);


--
-- Name: organizations organizations_pkey; Type: CONSTRAINT; Schema: public; Owner: slath
--

ALTER TABLE ONLY public.organizations
    ADD CONSTRAINT organizations_pkey PRIMARY KEY (id);


--
-- Name: password_resets password_resets_pkey; Type: CONSTRAINT; Schema: public; Owner: slath
--

ALTER TABLE ONLY public.password_resets
    ADD CONSTRAINT password_resets_pkey PRIMARY KEY (token_hash);


--
-- Name: push_events push_events_pkey; Type: CONSTRAINT; Schema: public; Owner: slath
--

ALTER TABLE ONLY public.push_events
    ADD CONSTRAINT push_events_pkey PRIMARY KEY (id);


--
-- Name: recovery_codes recovery_codes_pkey; Type: CONSTRAINT; Schema: public; Owner: slath
--

ALTER TABLE ONLY public.recovery_codes
    ADD CONSTRAINT recovery_codes_pkey PRIMARY KEY (id);


--
-- Name: ref_updates ref_updates_pkey; Type: CONSTRAINT; Schema: public; Owner: slath
--

ALTER TABLE ONLY public.ref_updates
    ADD CONSTRAINT ref_updates_pkey PRIMARY KEY (push_id, seq);


--
-- Name: repo_collaborators repo_collaborators_pkey; Type: CONSTRAINT; Schema: public; Owner: slath
--

ALTER TABLE ONLY public.repo_collaborators
    ADD CONSTRAINT repo_collaborators_pkey PRIMARY KEY (repo_id, user_id);


--
-- Name: repo_imports repo_imports_pkey; Type: CONSTRAINT; Schema: public; Owner: slath
--

ALTER TABLE ONLY public.repo_imports
    ADD CONSTRAINT repo_imports_pkey PRIMARY KEY (repo_id);


--
-- Name: repo_redirects repo_redirects_pkey; Type: CONSTRAINT; Schema: public; Owner: slath
--

ALTER TABLE ONLY public.repo_redirects
    ADD CONSTRAINT repo_redirects_pkey PRIMARY KEY (namespace_id, name);


--
-- Name: repo_team_access repo_team_access_pkey; Type: CONSTRAINT; Schema: public; Owner: slath
--

ALTER TABLE ONLY public.repo_team_access
    ADD CONSTRAINT repo_team_access_pkey PRIMARY KEY (repo_id, team_id);


--
-- Name: repositories repositories_namespace_id_name_key; Type: CONSTRAINT; Schema: public; Owner: slath
--

ALTER TABLE ONLY public.repositories
    ADD CONSTRAINT repositories_namespace_id_name_key UNIQUE (namespace_id, name);


--
-- Name: repositories repositories_pkey; Type: CONSTRAINT; Schema: public; Owner: slath
--

ALTER TABLE ONLY public.repositories
    ADD CONSTRAINT repositories_pkey PRIMARY KEY (id);


--
-- Name: reserved_names reserved_names_pkey; Type: CONSTRAINT; Schema: public; Owner: slath
--

ALTER TABLE ONLY public.reserved_names
    ADD CONSTRAINT reserved_names_pkey PRIMARY KEY (name);


--
-- Name: schema_info schema_info_pkey; Type: CONSTRAINT; Schema: public; Owner: slath
--

ALTER TABLE ONLY public.schema_info
    ADD CONSTRAINT schema_info_pkey PRIMARY KEY (singleton);


--
-- Name: sessions sessions_pkey; Type: CONSTRAINT; Schema: public; Owner: slath
--

ALTER TABLE ONLY public.sessions
    ADD CONSTRAINT sessions_pkey PRIMARY KEY (id_hash);


--
-- Name: ssh_keys ssh_keys_fingerprint_sha256_key; Type: CONSTRAINT; Schema: public; Owner: slath
--

ALTER TABLE ONLY public.ssh_keys
    ADD CONSTRAINT ssh_keys_fingerprint_sha256_key UNIQUE (fingerprint_sha256);


--
-- Name: ssh_keys ssh_keys_pkey; Type: CONSTRAINT; Schema: public; Owner: slath
--

ALTER TABLE ONLY public.ssh_keys
    ADD CONSTRAINT ssh_keys_pkey PRIMARY KEY (id);


--
-- Name: subscriptions subscriptions_pkey; Type: CONSTRAINT; Schema: public; Owner: slath
--

ALTER TABLE ONLY public.subscriptions
    ADD CONSTRAINT subscriptions_pkey PRIMARY KEY (user_id, subject_kind, subject_id);


--
-- Name: team_members team_members_pkey; Type: CONSTRAINT; Schema: public; Owner: slath
--

ALTER TABLE ONLY public.team_members
    ADD CONSTRAINT team_members_pkey PRIMARY KEY (team_id, user_id);


--
-- Name: teams teams_org_id_name_key; Type: CONSTRAINT; Schema: public; Owner: slath
--

ALTER TABLE ONLY public.teams
    ADD CONSTRAINT teams_org_id_name_key UNIQUE (org_id, name);


--
-- Name: teams teams_pkey; Type: CONSTRAINT; Schema: public; Owner: slath
--

ALTER TABLE ONLY public.teams
    ADD CONSTRAINT teams_pkey PRIMARY KEY (id);


--
-- Name: timeline_events timeline_events_pkey; Type: CONSTRAINT; Schema: public; Owner: slath
--

ALTER TABLE ONLY public.timeline_events
    ADD CONSTRAINT timeline_events_pkey PRIMARY KEY (id);


--
-- Name: totp_credentials totp_credentials_pkey; Type: CONSTRAINT; Schema: public; Owner: slath
--

ALTER TABLE ONLY public.totp_credentials
    ADD CONSTRAINT totp_credentials_pkey PRIMARY KEY (user_id);


--
-- Name: upgrade_history upgrade_history_pkey; Type: CONSTRAINT; Schema: public; Owner: slath
--

ALTER TABLE ONLY public.upgrade_history
    ADD CONSTRAINT upgrade_history_pkey PRIMARY KEY (id);


--
-- Name: users users_namespace_id_key; Type: CONSTRAINT; Schema: public; Owner: slath
--

ALTER TABLE ONLY public.users
    ADD CONSTRAINT users_namespace_id_key UNIQUE (namespace_id);


--
-- Name: users users_pkey; Type: CONSTRAINT; Schema: public; Owner: slath
--

ALTER TABLE ONLY public.users
    ADD CONSTRAINT users_pkey PRIMARY KEY (id);


--
-- Name: webauthn_ceremonies webauthn_ceremonies_pkey; Type: CONSTRAINT; Schema: public; Owner: slath
--

ALTER TABLE ONLY public.webauthn_ceremonies
    ADD CONSTRAINT webauthn_ceremonies_pkey PRIMARY KEY (id_hash);


--
-- Name: webauthn_credentials webauthn_credentials_credential_id_key; Type: CONSTRAINT; Schema: public; Owner: slath
--

ALTER TABLE ONLY public.webauthn_credentials
    ADD CONSTRAINT webauthn_credentials_credential_id_key UNIQUE (credential_id);


--
-- Name: webauthn_credentials webauthn_credentials_pkey; Type: CONSTRAINT; Schema: public; Owner: slath
--

ALTER TABLE ONLY public.webauthn_credentials
    ADD CONSTRAINT webauthn_credentials_pkey PRIMARY KEY (id);


--
-- Name: access_tokens_user_id_idx; Type: INDEX; Schema: public; Owner: slath
--

CREATE INDEX access_tokens_user_id_idx ON public.access_tokens USING btree (user_id);


--
-- Name: attachments_blob; Type: INDEX; Schema: public; Owner: slath
--

CREATE INDEX attachments_blob ON public.attachments USING btree (blob_sha256);


--
-- Name: attachments_repo; Type: INDEX; Schema: public; Owner: slath
--

CREATE INDEX attachments_repo ON public.attachments USING btree (repo_id, created_at);


--
-- Name: audit_events_created_at_idx; Type: INDEX; Schema: public; Owner: slath
--

CREATE INDEX audit_events_created_at_idx ON public.audit_events USING btree (created_at);


--
-- Name: blobs_unreferenced; Type: INDEX; Schema: public; Owner: slath
--

CREATE INDEX blobs_unreferenced ON public.blobs USING btree (unreferenced_since) WHERE (refcount = 0);


--
-- Name: comments_search; Type: INDEX; Schema: public; Owner: slath
--

CREATE INDEX comments_search ON public.comments USING gin (search);


--
-- Name: comments_subject; Type: INDEX; Schema: public; Owner: slath
--

CREATE INDEX comments_subject ON public.comments USING btree (subject_kind, subject_id, created_at);


--
-- Name: email_verifications_user; Type: INDEX; Schema: public; Owner: slath
--

CREATE INDEX email_verifications_user ON public.email_verifications USING btree (user_id);


--
-- Name: idempotency_keys_created_at_idx; Type: INDEX; Schema: public; Owner: slath
--

CREATE INDEX idempotency_keys_created_at_idx ON public.idempotency_keys USING btree (created_at);


--
-- Name: issue_assignees_user; Type: INDEX; Schema: public; Owner: slath
--

CREATE INDEX issue_assignees_user ON public.issue_assignees USING btree (user_id);


--
-- Name: issue_labels_label; Type: INDEX; Schema: public; Owner: slath
--

CREATE INDEX issue_labels_label ON public.issue_labels USING btree (label_id);


--
-- Name: issues_author; Type: INDEX; Schema: public; Owner: slath
--

CREATE INDEX issues_author ON public.issues USING btree (author_id);


--
-- Name: issues_repo_state; Type: INDEX; Schema: public; Owner: slath
--

CREATE INDEX issues_repo_state ON public.issues USING btree (repo_id, state, updated_at DESC);


--
-- Name: issues_search; Type: INDEX; Schema: public; Owner: slath
--

CREATE INDEX issues_search ON public.issues USING gin (search);


--
-- Name: jobs_queue_state_run_at_idx; Type: INDEX; Schema: public; Owner: slath
--

CREATE INDEX jobs_queue_state_run_at_idx ON public.jobs USING btree (queue, state, run_at);


--
-- Name: jobs_running_locked_until_idx; Type: INDEX; Schema: public; Owner: slath
--

CREATE INDEX jobs_running_locked_until_idx ON public.jobs USING btree (locked_until) WHERE (state = 'running'::text);


--
-- Name: login_attempts_window_start_idx; Type: INDEX; Schema: public; Owner: slath
--

CREATE INDEX login_attempts_window_start_idx ON public.login_attempts USING btree (window_start);


--
-- Name: login_challenges_expiry; Type: INDEX; Schema: public; Owner: slath
--

CREATE INDEX login_challenges_expiry ON public.login_challenges USING btree (expires_at);


--
-- Name: notifications_inbox; Type: INDEX; Schema: public; Owner: slath
--

CREATE INDEX notifications_inbox ON public.notifications USING btree (user_id, updated_at DESC);


--
-- Name: notifications_unread; Type: INDEX; Schema: public; Owner: slath
--

CREATE INDEX notifications_unread ON public.notifications USING btree (user_id) WHERE unread;


--
-- Name: oidc_identities_user; Type: INDEX; Schema: public; Owner: slath
--

CREATE INDEX oidc_identities_user ON public.oidc_identities USING btree (user_id);


--
-- Name: org_members_user_id_idx; Type: INDEX; Schema: public; Owner: slath
--

CREATE INDEX org_members_user_id_idx ON public.org_members USING btree (user_id);


--
-- Name: password_resets_user; Type: INDEX; Schema: public; Owner: slath
--

CREATE INDEX password_resets_user ON public.password_resets USING btree (user_id);


--
-- Name: push_events_repo_id_idx; Type: INDEX; Schema: public; Owner: slath
--

CREATE INDEX push_events_repo_id_idx ON public.push_events USING btree (repo_id, created_at DESC);


--
-- Name: recovery_codes_user; Type: INDEX; Schema: public; Owner: slath
--

CREATE INDEX recovery_codes_user ON public.recovery_codes USING btree (user_id) WHERE (used_at IS NULL);


--
-- Name: repo_collaborators_user_id_idx; Type: INDEX; Schema: public; Owner: slath
--

CREATE INDEX repo_collaborators_user_id_idx ON public.repo_collaborators USING btree (user_id);


--
-- Name: repo_imports_unfinished; Type: INDEX; Schema: public; Owner: slath
--

CREATE INDEX repo_imports_unfinished ON public.repo_imports USING btree (state) WHERE (state = ANY (ARRAY['queued'::text, 'running'::text]));


--
-- Name: repo_team_access_team; Type: INDEX; Schema: public; Owner: slath
--

CREATE INDEX repo_team_access_team ON public.repo_team_access USING btree (team_id);


--
-- Name: repositories_description_trgm; Type: INDEX; Schema: public; Owner: slath
--

CREATE INDEX repositories_description_trgm ON public.repositories USING gin (description public.gin_trgm_ops);


--
-- Name: repositories_fork_network_id_idx; Type: INDEX; Schema: public; Owner: slath
--

CREATE INDEX repositories_fork_network_id_idx ON public.repositories USING btree (fork_network_id);


--
-- Name: repositories_fork_parent_id_idx; Type: INDEX; Schema: public; Owner: slath
--

CREATE INDEX repositories_fork_parent_id_idx ON public.repositories USING btree (fork_parent_id);


--
-- Name: repositories_maintenance_due_idx; Type: INDEX; Schema: public; Owner: slath
--

CREATE INDEX repositories_maintenance_due_idx ON public.repositories USING btree (maintained_at) WHERE ((pushes_since_maintenance > 0) AND (state <> 'deleting'::text));


--
-- Name: repositories_name_trgm; Type: INDEX; Schema: public; Owner: slath
--

CREATE INDEX repositories_name_trgm ON public.repositories USING gin (((name)::text) public.gin_trgm_ops);


--
-- Name: repositories_public_idx; Type: INDEX; Schema: public; Owner: slath
--

CREATE INDEX repositories_public_idx ON public.repositories USING btree (updated_at DESC) WHERE ((visibility = 'public'::text) AND (state <> 'deleting'::text));


--
-- Name: sessions_expires_at_idx; Type: INDEX; Schema: public; Owner: slath
--

CREATE INDEX sessions_expires_at_idx ON public.sessions USING btree (expires_at);


--
-- Name: sessions_user_id_idx; Type: INDEX; Schema: public; Owner: slath
--

CREATE INDEX sessions_user_id_idx ON public.sessions USING btree (user_id);


--
-- Name: ssh_keys_user_id_idx; Type: INDEX; Schema: public; Owner: slath
--

CREATE INDEX ssh_keys_user_id_idx ON public.ssh_keys USING btree (user_id);


--
-- Name: subscriptions_subject; Type: INDEX; Schema: public; Owner: slath
--

CREATE INDEX subscriptions_subject ON public.subscriptions USING btree (subject_kind, subject_id, state);


--
-- Name: team_members_user; Type: INDEX; Schema: public; Owner: slath
--

CREATE INDEX team_members_user ON public.team_members USING btree (user_id);


--
-- Name: timeline_events_subject; Type: INDEX; Schema: public; Owner: slath
--

CREATE INDEX timeline_events_subject ON public.timeline_events USING btree (subject_kind, subject_id, created_at);


--
-- Name: totp_credentials_key_id; Type: INDEX; Schema: public; Owner: slath
--

CREATE INDEX totp_credentials_key_id ON public.totp_credentials USING btree (key_id);


--
-- Name: users_verified_email_key; Type: INDEX; Schema: public; Owner: slath
--

CREATE UNIQUE INDEX users_verified_email_key ON public.users USING btree (email) WHERE email_verified;


--
-- Name: webauthn_ceremonies_expiry; Type: INDEX; Schema: public; Owner: slath
--

CREATE INDEX webauthn_ceremonies_expiry ON public.webauthn_ceremonies USING btree (expires_at);


--
-- Name: webauthn_credentials_user; Type: INDEX; Schema: public; Owner: slath
--

CREATE INDEX webauthn_credentials_user ON public.webauthn_credentials USING btree (user_id);


--
-- Name: attachments attachments_blob_ref; Type: TRIGGER; Schema: public; Owner: slath
--

CREATE TRIGGER attachments_blob_ref AFTER INSERT OR DELETE ON public.attachments FOR EACH ROW EXECUTE FUNCTION public.slath_attachment_blob_ref();


--
-- Name: audit_events audit_events_append_only; Type: TRIGGER; Schema: public; Owner: slath
--

CREATE TRIGGER audit_events_append_only BEFORE DELETE OR UPDATE ON public.audit_events FOR EACH ROW EXECUTE FUNCTION public.audit_events_append_only();


--
-- Name: organizations organizations_avatar_blob_ref; Type: TRIGGER; Schema: public; Owner: slath
--

CREATE TRIGGER organizations_avatar_blob_ref AFTER INSERT OR DELETE OR UPDATE OF avatar_blob_sha256 ON public.organizations FOR EACH ROW EXECUTE FUNCTION public.slath_avatar_blob_ref();


--
-- Name: users users_avatar_blob_ref; Type: TRIGGER; Schema: public; Owner: slath
--

CREATE TRIGGER users_avatar_blob_ref AFTER INSERT OR DELETE OR UPDATE OF avatar_blob_sha256 ON public.users FOR EACH ROW EXECUTE FUNCTION public.slath_avatar_blob_ref();


--
-- Name: access_tokens access_tokens_user_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: slath
--

ALTER TABLE ONLY public.access_tokens
    ADD CONSTRAINT access_tokens_user_id_fkey FOREIGN KEY (user_id) REFERENCES public.users(id) ON DELETE CASCADE;


--
-- Name: attachments attachments_blob_sha256_fkey; Type: FK CONSTRAINT; Schema: public; Owner: slath
--

ALTER TABLE ONLY public.attachments
    ADD CONSTRAINT attachments_blob_sha256_fkey FOREIGN KEY (blob_sha256) REFERENCES public.blobs(sha256);


--
-- Name: attachments attachments_repo_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: slath
--

ALTER TABLE ONLY public.attachments
    ADD CONSTRAINT attachments_repo_id_fkey FOREIGN KEY (repo_id) REFERENCES public.repositories(id) ON DELETE CASCADE;


--
-- Name: attachments attachments_uploader_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: slath
--

ALTER TABLE ONLY public.attachments
    ADD CONSTRAINT attachments_uploader_id_fkey FOREIGN KEY (uploader_id) REFERENCES public.users(id) ON DELETE SET NULL;


--
-- Name: branch_protections branch_protections_repo_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: slath
--

ALTER TABLE ONLY public.branch_protections
    ADD CONSTRAINT branch_protections_repo_id_fkey FOREIGN KEY (repo_id) REFERENCES public.repositories(id) ON DELETE CASCADE;


--
-- Name: comments comments_author_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: slath
--

ALTER TABLE ONLY public.comments
    ADD CONSTRAINT comments_author_id_fkey FOREIGN KEY (author_id) REFERENCES public.users(id) ON DELETE SET NULL;


--
-- Name: comments comments_repo_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: slath
--

ALTER TABLE ONLY public.comments
    ADD CONSTRAINT comments_repo_id_fkey FOREIGN KEY (repo_id) REFERENCES public.repositories(id) ON DELETE CASCADE;


--
-- Name: email_verifications email_verifications_user_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: slath
--

ALTER TABLE ONLY public.email_verifications
    ADD CONSTRAINT email_verifications_user_id_fkey FOREIGN KEY (user_id) REFERENCES public.users(id) ON DELETE CASCADE;


--
-- Name: git_refs git_refs_repo_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: slath
--

ALTER TABLE ONLY public.git_refs
    ADD CONSTRAINT git_refs_repo_id_fkey FOREIGN KEY (repo_id) REFERENCES public.repositories(id) ON DELETE CASCADE;


--
-- Name: invitations invitations_created_by_fkey; Type: FK CONSTRAINT; Schema: public; Owner: slath
--

ALTER TABLE ONLY public.invitations
    ADD CONSTRAINT invitations_created_by_fkey FOREIGN KEY (created_by) REFERENCES public.users(id) ON DELETE SET NULL;


--
-- Name: invitations invitations_used_by_fkey; Type: FK CONSTRAINT; Schema: public; Owner: slath
--

ALTER TABLE ONLY public.invitations
    ADD CONSTRAINT invitations_used_by_fkey FOREIGN KEY (used_by) REFERENCES public.users(id) ON DELETE SET NULL;


--
-- Name: issue_assignees issue_assignees_issue_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: slath
--

ALTER TABLE ONLY public.issue_assignees
    ADD CONSTRAINT issue_assignees_issue_id_fkey FOREIGN KEY (issue_id) REFERENCES public.issues(id) ON DELETE CASCADE;


--
-- Name: issue_assignees issue_assignees_user_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: slath
--

ALTER TABLE ONLY public.issue_assignees
    ADD CONSTRAINT issue_assignees_user_id_fkey FOREIGN KEY (user_id) REFERENCES public.users(id) ON DELETE CASCADE;


--
-- Name: issue_labels issue_labels_issue_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: slath
--

ALTER TABLE ONLY public.issue_labels
    ADD CONSTRAINT issue_labels_issue_id_fkey FOREIGN KEY (issue_id) REFERENCES public.issues(id) ON DELETE CASCADE;


--
-- Name: issue_labels issue_labels_label_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: slath
--

ALTER TABLE ONLY public.issue_labels
    ADD CONSTRAINT issue_labels_label_id_fkey FOREIGN KEY (label_id) REFERENCES public.labels(id) ON DELETE CASCADE;


--
-- Name: issues issues_author_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: slath
--

ALTER TABLE ONLY public.issues
    ADD CONSTRAINT issues_author_id_fkey FOREIGN KEY (author_id) REFERENCES public.users(id) ON DELETE SET NULL;


--
-- Name: issues issues_milestone_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: slath
--

ALTER TABLE ONLY public.issues
    ADD CONSTRAINT issues_milestone_id_fkey FOREIGN KEY (milestone_id) REFERENCES public.milestones(id) ON DELETE SET NULL;


--
-- Name: issues issues_repo_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: slath
--

ALTER TABLE ONLY public.issues
    ADD CONSTRAINT issues_repo_id_fkey FOREIGN KEY (repo_id) REFERENCES public.repositories(id) ON DELETE CASCADE;


--
-- Name: labels labels_repo_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: slath
--

ALTER TABLE ONLY public.labels
    ADD CONSTRAINT labels_repo_id_fkey FOREIGN KEY (repo_id) REFERENCES public.repositories(id) ON DELETE CASCADE;


--
-- Name: login_challenges login_challenges_user_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: slath
--

ALTER TABLE ONLY public.login_challenges
    ADD CONSTRAINT login_challenges_user_id_fkey FOREIGN KEY (user_id) REFERENCES public.users(id) ON DELETE CASCADE;


--
-- Name: milestones milestones_repo_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: slath
--

ALTER TABLE ONLY public.milestones
    ADD CONSTRAINT milestones_repo_id_fkey FOREIGN KEY (repo_id) REFERENCES public.repositories(id) ON DELETE CASCADE;


--
-- Name: notifications notifications_repo_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: slath
--

ALTER TABLE ONLY public.notifications
    ADD CONSTRAINT notifications_repo_id_fkey FOREIGN KEY (repo_id) REFERENCES public.repositories(id) ON DELETE CASCADE;


--
-- Name: notifications notifications_user_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: slath
--

ALTER TABLE ONLY public.notifications
    ADD CONSTRAINT notifications_user_id_fkey FOREIGN KEY (user_id) REFERENCES public.users(id) ON DELETE CASCADE;


--
-- Name: oidc_identities oidc_identities_user_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: slath
--

ALTER TABLE ONLY public.oidc_identities
    ADD CONSTRAINT oidc_identities_user_id_fkey FOREIGN KEY (user_id) REFERENCES public.users(id) ON DELETE CASCADE;


--
-- Name: oidc_states oidc_states_user_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: slath
--

ALTER TABLE ONLY public.oidc_states
    ADD CONSTRAINT oidc_states_user_id_fkey FOREIGN KEY (user_id) REFERENCES public.users(id) ON DELETE CASCADE;


--
-- Name: org_members org_members_org_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: slath
--

ALTER TABLE ONLY public.org_members
    ADD CONSTRAINT org_members_org_id_fkey FOREIGN KEY (org_id) REFERENCES public.organizations(id) ON DELETE CASCADE;


--
-- Name: org_members org_members_user_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: slath
--

ALTER TABLE ONLY public.org_members
    ADD CONSTRAINT org_members_user_id_fkey FOREIGN KEY (user_id) REFERENCES public.users(id) ON DELETE CASCADE;


--
-- Name: organizations organizations_avatar_blob_sha256_fkey; Type: FK CONSTRAINT; Schema: public; Owner: slath
--

ALTER TABLE ONLY public.organizations
    ADD CONSTRAINT organizations_avatar_blob_sha256_fkey FOREIGN KEY (avatar_blob_sha256) REFERENCES public.blobs(sha256);


--
-- Name: organizations organizations_namespace_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: slath
--

ALTER TABLE ONLY public.organizations
    ADD CONSTRAINT organizations_namespace_id_fkey FOREIGN KEY (namespace_id) REFERENCES public.namespaces(id);


--
-- Name: password_resets password_resets_user_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: slath
--

ALTER TABLE ONLY public.password_resets
    ADD CONSTRAINT password_resets_user_id_fkey FOREIGN KEY (user_id) REFERENCES public.users(id) ON DELETE CASCADE;


--
-- Name: push_events push_events_pusher_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: slath
--

ALTER TABLE ONLY public.push_events
    ADD CONSTRAINT push_events_pusher_id_fkey FOREIGN KEY (pusher_id) REFERENCES public.users(id) ON DELETE SET NULL;


--
-- Name: push_events push_events_repo_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: slath
--

ALTER TABLE ONLY public.push_events
    ADD CONSTRAINT push_events_repo_id_fkey FOREIGN KEY (repo_id) REFERENCES public.repositories(id) ON DELETE CASCADE;


--
-- Name: recovery_codes recovery_codes_user_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: slath
--

ALTER TABLE ONLY public.recovery_codes
    ADD CONSTRAINT recovery_codes_user_id_fkey FOREIGN KEY (user_id) REFERENCES public.users(id) ON DELETE CASCADE;


--
-- Name: ref_updates ref_updates_push_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: slath
--

ALTER TABLE ONLY public.ref_updates
    ADD CONSTRAINT ref_updates_push_id_fkey FOREIGN KEY (push_id) REFERENCES public.push_events(id) ON DELETE CASCADE;


--
-- Name: repo_collaborators repo_collaborators_repo_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: slath
--

ALTER TABLE ONLY public.repo_collaborators
    ADD CONSTRAINT repo_collaborators_repo_id_fkey FOREIGN KEY (repo_id) REFERENCES public.repositories(id) ON DELETE CASCADE;


--
-- Name: repo_collaborators repo_collaborators_user_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: slath
--

ALTER TABLE ONLY public.repo_collaborators
    ADD CONSTRAINT repo_collaborators_user_id_fkey FOREIGN KEY (user_id) REFERENCES public.users(id) ON DELETE CASCADE;


--
-- Name: repo_imports repo_imports_repo_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: slath
--

ALTER TABLE ONLY public.repo_imports
    ADD CONSTRAINT repo_imports_repo_id_fkey FOREIGN KEY (repo_id) REFERENCES public.repositories(id) ON DELETE CASCADE;


--
-- Name: repo_imports repo_imports_requested_by_fkey; Type: FK CONSTRAINT; Schema: public; Owner: slath
--

ALTER TABLE ONLY public.repo_imports
    ADD CONSTRAINT repo_imports_requested_by_fkey FOREIGN KEY (requested_by) REFERENCES public.users(id) ON DELETE SET NULL;


--
-- Name: repo_redirects repo_redirects_namespace_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: slath
--

ALTER TABLE ONLY public.repo_redirects
    ADD CONSTRAINT repo_redirects_namespace_id_fkey FOREIGN KEY (namespace_id) REFERENCES public.namespaces(id);


--
-- Name: repo_redirects repo_redirects_repo_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: slath
--

ALTER TABLE ONLY public.repo_redirects
    ADD CONSTRAINT repo_redirects_repo_id_fkey FOREIGN KEY (repo_id) REFERENCES public.repositories(id) ON DELETE CASCADE;


--
-- Name: repo_team_access repo_team_access_repo_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: slath
--

ALTER TABLE ONLY public.repo_team_access
    ADD CONSTRAINT repo_team_access_repo_id_fkey FOREIGN KEY (repo_id) REFERENCES public.repositories(id) ON DELETE CASCADE;


--
-- Name: repo_team_access repo_team_access_team_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: slath
--

ALTER TABLE ONLY public.repo_team_access
    ADD CONSTRAINT repo_team_access_team_id_fkey FOREIGN KEY (team_id) REFERENCES public.teams(id) ON DELETE CASCADE;


--
-- Name: repositories repositories_fork_parent_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: slath
--

ALTER TABLE ONLY public.repositories
    ADD CONSTRAINT repositories_fork_parent_id_fkey FOREIGN KEY (fork_parent_id) REFERENCES public.repositories(id) ON DELETE SET NULL;


--
-- Name: repositories repositories_namespace_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: slath
--

ALTER TABLE ONLY public.repositories
    ADD CONSTRAINT repositories_namespace_id_fkey FOREIGN KEY (namespace_id) REFERENCES public.namespaces(id);


--
-- Name: sessions sessions_user_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: slath
--

ALTER TABLE ONLY public.sessions
    ADD CONSTRAINT sessions_user_id_fkey FOREIGN KEY (user_id) REFERENCES public.users(id) ON DELETE CASCADE;


--
-- Name: ssh_keys ssh_keys_user_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: slath
--

ALTER TABLE ONLY public.ssh_keys
    ADD CONSTRAINT ssh_keys_user_id_fkey FOREIGN KEY (user_id) REFERENCES public.users(id) ON DELETE CASCADE;


--
-- Name: subscriptions subscriptions_repo_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: slath
--

ALTER TABLE ONLY public.subscriptions
    ADD CONSTRAINT subscriptions_repo_id_fkey FOREIGN KEY (repo_id) REFERENCES public.repositories(id) ON DELETE CASCADE;


--
-- Name: subscriptions subscriptions_user_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: slath
--

ALTER TABLE ONLY public.subscriptions
    ADD CONSTRAINT subscriptions_user_id_fkey FOREIGN KEY (user_id) REFERENCES public.users(id) ON DELETE CASCADE;


--
-- Name: team_members team_members_team_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: slath
--

ALTER TABLE ONLY public.team_members
    ADD CONSTRAINT team_members_team_id_fkey FOREIGN KEY (team_id) REFERENCES public.teams(id) ON DELETE CASCADE;


--
-- Name: team_members team_members_user_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: slath
--

ALTER TABLE ONLY public.team_members
    ADD CONSTRAINT team_members_user_id_fkey FOREIGN KEY (user_id) REFERENCES public.users(id) ON DELETE CASCADE;


--
-- Name: teams teams_org_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: slath
--

ALTER TABLE ONLY public.teams
    ADD CONSTRAINT teams_org_id_fkey FOREIGN KEY (org_id) REFERENCES public.organizations(id) ON DELETE CASCADE;


--
-- Name: timeline_events timeline_events_actor_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: slath
--

ALTER TABLE ONLY public.timeline_events
    ADD CONSTRAINT timeline_events_actor_id_fkey FOREIGN KEY (actor_id) REFERENCES public.users(id) ON DELETE SET NULL;


--
-- Name: timeline_events timeline_events_repo_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: slath
--

ALTER TABLE ONLY public.timeline_events
    ADD CONSTRAINT timeline_events_repo_id_fkey FOREIGN KEY (repo_id) REFERENCES public.repositories(id) ON DELETE CASCADE;


--
-- Name: totp_credentials totp_credentials_key_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: slath
--

ALTER TABLE ONLY public.totp_credentials
    ADD CONSTRAINT totp_credentials_key_id_fkey FOREIGN KEY (key_id) REFERENCES public.encryption_keys(key_id);


--
-- Name: totp_credentials totp_credentials_user_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: slath
--

ALTER TABLE ONLY public.totp_credentials
    ADD CONSTRAINT totp_credentials_user_id_fkey FOREIGN KEY (user_id) REFERENCES public.users(id) ON DELETE CASCADE;


--
-- Name: users users_avatar_blob_fkey; Type: FK CONSTRAINT; Schema: public; Owner: slath
--

ALTER TABLE ONLY public.users
    ADD CONSTRAINT users_avatar_blob_fkey FOREIGN KEY (avatar_blob_sha256) REFERENCES public.blobs(sha256);


--
-- Name: users users_namespace_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: slath
--

ALTER TABLE ONLY public.users
    ADD CONSTRAINT users_namespace_id_fkey FOREIGN KEY (namespace_id) REFERENCES public.namespaces(id);


--
-- Name: webauthn_ceremonies webauthn_ceremonies_user_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: slath
--

ALTER TABLE ONLY public.webauthn_ceremonies
    ADD CONSTRAINT webauthn_ceremonies_user_id_fkey FOREIGN KEY (user_id) REFERENCES public.users(id) ON DELETE CASCADE;


--
-- Name: webauthn_credentials webauthn_credentials_user_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: slath
--

ALTER TABLE ONLY public.webauthn_credentials
    ADD CONSTRAINT webauthn_credentials_user_id_fkey FOREIGN KEY (user_id) REFERENCES public.users(id) ON DELETE CASCADE;


--
-- PostgreSQL database dump complete
--

\unrestrict VQtY69eNhzPfID1OJYKxRRUsB7hpgXanBD3xQCJFdZXAgpNyJZPYhngqMENtArr

