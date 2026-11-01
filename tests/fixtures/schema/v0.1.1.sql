-- SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- Schema and data of Slath 0.1.1 (schema 9), written by `just schema-fixture`.
-- Users: alice, bob, root-admin (password "slath test password"). Don't edit.

--
-- PostgreSQL database dump
--

\restrict PqnvGyuZ73eZ59TmLUQcPsEjF0Ye5TbyOKh7NIl4AHh4hmbFQv1rndNanMs4r3V

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
-- Name: EXTENSION citext; Type: COMMENT; Schema: -; Owner: -
--

COMMENT ON EXTENSION citext IS 'data type for case-insensitive character strings';


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
-- Name: instance_settings; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.instance_settings (
    key text NOT NULL,
    value jsonb NOT NULL,
    updated_at timestamp with time zone DEFAULT now() NOT NULL
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
-- Name: login_attempts; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.login_attempts (
    key text NOT NULL,
    window_start timestamp with time zone NOT NULL,
    count integer DEFAULT 0 NOT NULL,
    CONSTRAINT login_attempts_key_check CHECK ((length(key) <= 200))
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
    CONSTRAINT organizations_default_member_role_check CHECK ((default_member_role = ANY (ARRAY['none'::text, 'read'::text]))),
    CONSTRAINT organizations_description_check CHECK ((length(description) <= 500)),
    CONSTRAINT organizations_display_name_check CHECK ((length(display_name) <= 100))
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
-- Name: repo_redirects; Type: TABLE; Schema: public; Owner: -
--

CREATE TABLE public.repo_redirects (
    namespace_id uuid NOT NULL,
    name public.citext NOT NULL,
    repo_id uuid NOT NULL,
    created_at timestamp with time zone DEFAULT now() NOT NULL
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
    CONSTRAINT repositories_archived_reason_check CHECK ((archived_reason = ANY (ARRAY['user'::text, 'lost_parent_access'::text]))),
    CONSTRAINT repositories_default_branch_check CHECK (((length(default_branch) >= 1) AND (length(default_branch) <= 255))),
    CONSTRAINT repositories_description_check CHECK ((length(description) <= 500)),
    CONSTRAINT repositories_state_check CHECK ((state = ANY (ARRAY['active'::text, 'archived'::text, 'deleting'::text]))),
    CONSTRAINT repositories_visibility_check CHECK ((visibility = ANY (ARRAY['public'::text, 'private'::text])))
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


--
-- Name: jobs id; Type: DEFAULT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.jobs ALTER COLUMN id SET DEFAULT nextval('public.jobs_id_seq'::regclass);


--
-- Data for Name: _sqlx_migrations; Type: TABLE DATA; Schema: public; Owner: -
--

COPY public._sqlx_migrations (version, description, installed_on, success, checksum, execution_time) FROM stdin;
1	foundation	2026-11-01 10:19:51.433929+00	t	\\xd33ee3413e24792c0944e0147487310b878147391c3f1f8d058cfbdd50ac3f26cc9d362f61b0b5080e49cbf9674f8a6a	36253247
2	sessions	2026-11-01 10:19:51.471165+00	t	\\xbb6c166b89823c9d6f520c51e35d4a822ea6fcb61b8867110c6edac424e46bac6facac324dc52834a169dd98d4aaba49	6877766
3	access tokens	2026-11-01 10:19:51.478612+00	t	\\xf301152e64bc266d37146a892bd84566c5325f462f76cb396b09465221140ad905e2891d6cfcaede2a57a91a405ab4c7	4710379
4	ssh keys	2026-11-01 10:19:51.48366+00	t	\\xa38cbfa2b5f8449425974aac945ec6d726dff155bf1894350dac861aec1c5cb1079456c5dd30ce4054ed60d5a002a3d7	5041907
5	organizations	2026-11-01 10:19:51.489406+00	t	\\xce9a9b09ac147403c62660cabdb029bded5c22463e6f116a0c0a7dc301dcd411216d86a46c9cdcc4ff8d22741fb05612	7312660
6	repositories	2026-11-01 10:19:51.497296+00	t	\\x2d45e0f09a670784913372e54599c58495a5e6e5262ef59024b7cd3528b546d07052117cc66f0133476495ecc47ad927	20571392
7	idempotency keys	2026-11-01 10:19:51.518583+00	t	\\x176f61cb4640020001d7ae19cc9b038f70d37fe5a0e94f1cf9b1ed9a25b632c340a6d6fa77c850a588d3151164b66c2b	3934828
8	repo maintenance	2026-11-01 10:19:51.522875+00	t	\\x59525f963724f9333c4d62586341526d0497f399e92ba70d7ebb457dc0cef0e664056fbd93523401aaf29445e37df6f1	3550439
9	appearance	2026-11-01 10:19:51.526842+00	t	\\x28a35cb29bd16eaf97d79bc19e4f04230f4bfad09d631dfa87aa105eba1d1f2a1d4f837c4f41dda3d6f25b381be1b991	3071151
\.


--
-- Data for Name: access_tokens; Type: TABLE DATA; Schema: public; Owner: -
--

COPY public.access_tokens (id, user_id, name, token_prefix, token_hash, scopes, repo_ids, expires_at, last_used_at, revoked_at, created_at) FROM stdin;
01a1969f-69d7-74f7-9eb1-d33632a890c0	01a1969f-68df-75b9-8c8e-4ceb31bd12a0	tests	MbOkw9TY	\\xc182476b38d17cea64abdb49aa26bbf878c415a7c4ef677030cfbbb0fc489907	{repo:write,user:read}	\N	2026-12-01 10:19:52.15141+00	2026-11-01 10:19:52.849696+00	\N	2026-11-01 10:19:52.152336+00
01a1969f-6e94-7710-87cf-86a59e1770fc	01a1969f-6af7-7466-a978-a89adfb6d7aa	ci	xteXxvVu	\\x1686341daabe4ef636d0c45a590e7701d0ac10909ed8b341b389ac2d3d525417	{repo:read}	\N	2026-12-01 10:19:53.364292+00	\N	\N	2026-11-01 10:19:53.366989+00
\.


--
-- Data for Name: audit_events; Type: TABLE DATA; Schema: public; Owner: -
--

COPY public.audit_events (id, actor_id, actor_ip, action, target_kind, target_id, repo_id, org_id, data, created_at) FROM stdin;
01a1969f-68e3-752d-9d5e-f499fd4aa43b	\N	\N	user.create	user	01a1969f-68df-75b9-8c8e-4ceb31bd12a0	\N	\N	{"admin": false, "username": "alice"}	2026-11-01 10:19:51.90883+00
01a1969f-693c-73f8-a2f1-e563aef4d758	01a1969f-68df-75b9-8c8e-4ceb31bd12a0	127.0.0.1	login.success	user	01a1969f-68df-75b9-8c8e-4ceb31bd12a0	\N	\N	{}	2026-11-01 10:19:51.998704+00
01a1969f-6982-7530-8ad6-2336582f65cd	01a1969f-68df-75b9-8c8e-4ceb31bd12a0	127.0.0.1	repo.create	repository	01a1969f-6951-75f5-b0c1-be51e82ce19a	01a1969f-6951-75f5-b0c1-be51e82ce19a	\N	{"name": "alice/demo", "visibility": "private"}	2026-11-01 10:19:52.066844+00
01a1969f-69c6-7715-976e-54541b8ba847	01a1969f-68df-75b9-8c8e-4ceb31bd12a0	127.0.0.1	login.success	user	01a1969f-68df-75b9-8c8e-4ceb31bd12a0	\N	\N	{}	2026-11-01 10:19:52.135897+00
01a1969f-69d9-7323-9315-03db9c44c505	01a1969f-68df-75b9-8c8e-4ceb31bd12a0	127.0.0.1	token.create	token	01a1969f-69d7-74f7-9eb1-d33632a890c0	\N	\N	{"name": "tests", "scopes": ["repo:write", "user:read"], "expires_in_days": 30}	2026-11-01 10:19:52.154199+00
01a1969f-6afb-74dd-972a-cbcd6e909ca5	\N	\N	user.create	user	01a1969f-6af7-7466-a978-a89adfb6d7aa	\N	\N	{"admin": false, "username": "bob"}	2026-11-01 10:19:52.445049+00
01a1969f-6c0f-75d4-ae52-27b7bb66bcfd	\N	\N	user.create	user	01a1969f-6c0b-7411-bf37-b9b5e549293b	\N	\N	{"admin": true, "username": "root-admin"}	2026-11-01 10:19:52.720878+00
01a1969f-6d37-76cb-81f4-412020d206c9	01a1969f-68df-75b9-8c8e-4ceb31bd12a0	127.0.0.1	login.success	user	01a1969f-68df-75b9-8c8e-4ceb31bd12a0	\N	\N	{}	2026-11-01 10:19:53.015672+00
01a1969f-6d45-7389-9a6f-7ebf3da44ce3	01a1969f-68df-75b9-8c8e-4ceb31bd12a0	127.0.0.1	org.create	organization	01a1969f-6d41-74cd-bd7d-54d3bdcfb20f	\N	01a1969f-6d41-74cd-bd7d-54d3bdcfb20f	{"name": "acme"}	2026-11-01 10:19:53.030382+00
01a1969f-6d5d-77b9-87fe-6322ea26d24e	01a1969f-68df-75b9-8c8e-4ceb31bd12a0	127.0.0.1	org.member.set	organization	01a1969f-6d41-74cd-bd7d-54d3bdcfb20f	\N	01a1969f-6d41-74cd-bd7d-54d3bdcfb20f	{"role": "member", "username": "bob"}	2026-11-01 10:19:53.054174+00
01a1969f-6d9c-704b-b31d-93b96817910e	01a1969f-68df-75b9-8c8e-4ceb31bd12a0	127.0.0.1	repo.create	repository	01a1969f-6d69-757f-b897-f2f79d3f2fbe	01a1969f-6d69-757f-b897-f2f79d3f2fbe	01a1969f-6d41-74cd-bd7d-54d3bdcfb20f	{"name": "acme/tools", "visibility": "public"}	2026-11-01 10:19:53.118058+00
01a1969f-6db1-7269-a4ed-9b80d8e64c4b	01a1969f-68df-75b9-8c8e-4ceb31bd12a0	127.0.0.1	repo.collaborator.set	repository	01a1969f-6951-75f5-b0c1-be51e82ce19a	01a1969f-6951-75f5-b0c1-be51e82ce19a	\N	{"role": "write", "username": "bob"}	2026-11-01 10:19:53.137788+00
01a1969f-6dee-7090-a45f-80fa24cae773	01a1969f-68df-75b9-8c8e-4ceb31bd12a0	127.0.0.1	repo.create	repository	01a1969f-6dbc-72b7-b967-02f876557d21	01a1969f-6dbc-72b7-b967-02f876557d21	\N	{"name": "alice/renamed", "visibility": "private"}	2026-11-01 10:19:53.199025+00
01a1969f-6e04-74b7-bc41-85a4b88d5ebb	01a1969f-68df-75b9-8c8e-4ceb31bd12a0	127.0.0.1	repo.rename	repository	01a1969f-6dbc-72b7-b967-02f876557d21	01a1969f-6dbc-72b7-b967-02f876557d21	\N	{"to": "new-name", "from": "renamed"}	2026-11-01 10:19:53.220775+00
01a1969f-6e47-73fc-884a-51c511204eec	01a1969f-6af7-7466-a978-a89adfb6d7aa	127.0.0.1	login.success	user	01a1969f-6af7-7466-a978-a89adfb6d7aa	\N	\N	{}	2026-11-01 10:19:53.288176+00
01a1969f-6e56-7256-92a5-a7e095d66983	01a1969f-6af7-7466-a978-a89adfb6d7aa	127.0.0.1	ssh_key.add	ssh_key	01a1969f-6e54-7150-95e8-d266aefd27b3	\N	\N	{"title": "test key", "fingerprint": "SHA256:fh4ATFvLFM+W1TfSnry72WccMBo74BXeLohXb6E5SBI"}	2026-11-01 10:19:53.302885+00
01a1969f-6e88-704b-b765-f7dbdb77090e	01a1969f-6af7-7466-a978-a89adfb6d7aa	127.0.0.1	login.success	user	01a1969f-6af7-7466-a978-a89adfb6d7aa	\N	\N	{}	2026-11-01 10:19:53.353738+00
01a1969f-6e98-76f5-a97c-5de930b6a047	01a1969f-6af7-7466-a978-a89adfb6d7aa	127.0.0.1	token.create	token	01a1969f-6e94-7710-87cf-86a59e1770fc	\N	\N	{"name": "ci", "scopes": ["repo:read"], "expires_in_days": 30}	2026-11-01 10:19:53.370166+00
\.


--
-- Data for Name: git_refs; Type: TABLE DATA; Schema: public; Owner: -
--

COPY public.git_refs (repo_id, name, oid, updated_at) FROM stdin;
01a1969f-6951-75f5-b0c1-be51e82ce19a	\\x726566732f68656164732f6d61696e	e31a2bdcfa4366d63eec483681f2e7bef2528dd9	2026-11-01 10:19:52.933443+00
01a1969f-6951-75f5-b0c1-be51e82ce19a	\\x726566732f746167732f72656c656173652f322e30	822b6c5fab6d985b4dbf8caccf6ca5db811ab916	2026-11-01 10:19:52.933443+00
01a1969f-6951-75f5-b0c1-be51e82ce19a	\\x726566732f746167732f747265652d746167	b952f8c74fe4b4f428492d75c1e227657be2bf48	2026-11-01 10:19:52.933443+00
01a1969f-6951-75f5-b0c1-be51e82ce19a	\\x726566732f746167732f76312e30	032aee9a437815204df8a21bbc7371dcec2bd0e3	2026-11-01 10:19:52.933443+00
01a1969f-6951-75f5-b0c1-be51e82ce19a	\\x726566732f746167732f76312e302d6c69676874	3f3ec41f7a4fefa97602911c0258528cc2ad3d25	2026-11-01 10:19:52.933443+00
01a1969f-6951-75f5-b0c1-be51e82ce19a	\\x726566732f746167732f76322e302d6d657461	e1e0b1fbb57d0584f431150d048088a9e1508dbe	2026-11-01 10:19:52.933443+00
\.


--
-- Data for Name: idempotency_keys; Type: TABLE DATA; Schema: public; Owner: -
--

COPY public.idempotency_keys (principal_id, key, method, path, request_sha256, status, response, created_at) FROM stdin;
\.


--
-- Data for Name: instance_settings; Type: TABLE DATA; Schema: public; Owner: -
--

COPY public.instance_settings (key, value, updated_at) FROM stdin;
\.


--
-- Data for Name: jobs; Type: TABLE DATA; Schema: public; Owner: -
--

COPY public.jobs (id, queue, kind, payload, dedupe_key, state, run_at, attempts, max_attempts, locked_by, locked_until, last_error, created_at, finished_at) FROM stdin;
1	default	ref.updated	{"push_id": "01a1969f-6ce5-7400-b013-b9a10f0b5707", "repo_id": "01a1969f-6951-75f5-b0c1-be51e82ce19a"}	ref.updated:01a1969f-6ce5-7400-b013-b9a10f0b5707	done	2026-11-01 10:19:52.933443+00	1	8	\N	\N	\N	2026-11-01 10:19:52.933443+00	2026-11-01 10:19:52.953621+00
\.


--
-- Data for Name: login_attempts; Type: TABLE DATA; Schema: public; Owner: -
--

COPY public.login_attempts (key, window_start, count) FROM stdin;
\.


--
-- Data for Name: namespaces; Type: TABLE DATA; Schema: public; Owner: -
--

COPY public.namespaces (id, name, kind, created_at) FROM stdin;
01a1969f-68df-75b9-8c8e-4cea53f90da3	alice	user	2026-11-01 10:19:51.900285+00
01a1969f-6af7-7466-a978-a89988510aa5	bob	user	2026-11-01 10:19:52.436164+00
01a1969f-6c0b-7411-bf37-b9b4b3b84f1e	root-admin	user	2026-11-01 10:19:52.711979+00
01a1969f-6d41-74cd-bd7d-54d2f567ad4b	acme	org	2026-11-01 10:19:53.023553+00
\.


--
-- Data for Name: org_members; Type: TABLE DATA; Schema: public; Owner: -
--

COPY public.org_members (org_id, user_id, role, created_at) FROM stdin;
01a1969f-6d41-74cd-bd7d-54d3bdcfb20f	01a1969f-68df-75b9-8c8e-4ceb31bd12a0	owner	2026-11-01 10:19:53.023553+00
01a1969f-6d41-74cd-bd7d-54d3bdcfb20f	01a1969f-6af7-7466-a978-a89adfb6d7aa	member	2026-11-01 10:19:53.050476+00
\.


--
-- Data for Name: organizations; Type: TABLE DATA; Schema: public; Owner: -
--

COPY public.organizations (id, namespace_id, display_name, description, default_member_role, created_at, updated_at) FROM stdin;
01a1969f-6d41-74cd-bd7d-54d3bdcfb20f	01a1969f-6d41-74cd-bd7d-54d2f567ad4b	Acme	\N	none	2026-11-01 10:19:53.023553+00	2026-11-01 10:19:53.023553+00
\.


--
-- Data for Name: push_events; Type: TABLE DATA; Schema: public; Owner: -
--

COPY public.push_events (id, repo_id, pusher_id, transport, created_at) FROM stdin;
01a1969f-6ce5-7400-b013-b9a10f0b5707	01a1969f-6951-75f5-b0c1-be51e82ce19a	01a1969f-68df-75b9-8c8e-4ceb31bd12a0	http	2026-11-01 10:19:52.933443+00
\.


--
-- Data for Name: ref_updates; Type: TABLE DATA; Schema: public; Owner: -
--

COPY public.ref_updates (push_id, seq, name, old_oid, new_oid) FROM stdin;
01a1969f-6ce5-7400-b013-b9a10f0b5707	0	\\x726566732f68656164732f6d61696e	\N	e31a2bdcfa4366d63eec483681f2e7bef2528dd9
01a1969f-6ce5-7400-b013-b9a10f0b5707	1	\\x726566732f746167732f72656c656173652f322e30	\N	822b6c5fab6d985b4dbf8caccf6ca5db811ab916
01a1969f-6ce5-7400-b013-b9a10f0b5707	2	\\x726566732f746167732f747265652d746167	\N	b952f8c74fe4b4f428492d75c1e227657be2bf48
01a1969f-6ce5-7400-b013-b9a10f0b5707	3	\\x726566732f746167732f76312e30	\N	032aee9a437815204df8a21bbc7371dcec2bd0e3
01a1969f-6ce5-7400-b013-b9a10f0b5707	4	\\x726566732f746167732f76312e302d6c69676874	\N	3f3ec41f7a4fefa97602911c0258528cc2ad3d25
01a1969f-6ce5-7400-b013-b9a10f0b5707	5	\\x726566732f746167732f76322e302d6d657461	\N	e1e0b1fbb57d0584f431150d048088a9e1508dbe
\.


--
-- Data for Name: repo_collaborators; Type: TABLE DATA; Schema: public; Owner: -
--

COPY public.repo_collaborators (repo_id, user_id, role, created_at) FROM stdin;
01a1969f-6d69-757f-b897-f2f79d3f2fbe	01a1969f-68df-75b9-8c8e-4ceb31bd12a0	admin	2026-11-01 10:19:53.065974+00
01a1969f-6951-75f5-b0c1-be51e82ce19a	01a1969f-6af7-7466-a978-a89adfb6d7aa	write	2026-11-01 10:19:53.136343+00
\.


--
-- Data for Name: repo_redirects; Type: TABLE DATA; Schema: public; Owner: -
--

COPY public.repo_redirects (namespace_id, name, repo_id, created_at) FROM stdin;
01a1969f-68df-75b9-8c8e-4cea53f90da3	renamed	01a1969f-6dbc-72b7-b967-02f876557d21	2026-11-01 10:19:53.214038+00
\.


--
-- Data for Name: repositories; Type: TABLE DATA; Schema: public; Owner: -
--

COPY public.repositories (id, namespace_id, name, description, visibility, default_branch, state, fork_parent_id, fork_network_id, allow_forking, trust_bot_runs, auto_approve_external_for, issues_enabled, merge_requests_enabled, size_bytes, next_item_number, merge_strategies, default_merge_strategy, delete_source_branch_default, archived_reason, deleted_at, pushed_at, created_at, updated_at, pushes_since_maintenance, maintained_at, maintenance_runs, maintenance_lease_until) FROM stdin;
01a1969f-6951-75f5-b0c1-be51e82ce19a	01a1969f-68df-75b9-8c8e-4cea53f90da3	demo	\N	private	main	active	\N	01a1969f-6951-75f5-b0c1-be51e82ce19a	t	f	none	t	t	40960	1	{fast-forward,rebase,merge-commit,squash}	rebase	t	\N	\N	2026-11-01 10:19:52.933443+00	2026-11-01 10:19:52.018469+00	2026-11-01 10:19:52.018469+00	1	\N	0	\N
01a1969f-6d69-757f-b897-f2f79d3f2fbe	01a1969f-6d41-74cd-bd7d-54d2f567ad4b	tools	\N	public	main	active	\N	01a1969f-6d69-757f-b897-f2f79d3f2fbe	t	f	none	t	t	0	1	{fast-forward,rebase,merge-commit,squash}	rebase	t	\N	\N	\N	2026-11-01 10:19:53.065974+00	2026-11-01 10:19:53.065974+00	0	\N	0	\N
01a1969f-6dbc-72b7-b967-02f876557d21	01a1969f-68df-75b9-8c8e-4cea53f90da3	new-name	\N	private	main	active	\N	01a1969f-6dbc-72b7-b967-02f876557d21	t	f	none	t	t	0	1	{fast-forward,rebase,merge-commit,squash}	rebase	t	\N	\N	\N	2026-11-01 10:19:53.149006+00	2026-11-01 10:19:53.214038+00	0	\N	0	\N
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
9	t
\.


--
-- Data for Name: sessions; Type: TABLE DATA; Schema: public; Owner: -
--

COPY public.sessions (id_hash, user_id, created_at, last_seen_at, expires_at, auth_level, sudo_until, ip, user_agent) FROM stdin;
\\x285b05d2ed9eead934e5f698ad4794bf4fa6cc56000181ce84c4e62591d4acbf	01a1969f-68df-75b9-8c8e-4ceb31bd12a0	2026-11-01 10:19:51.99433+00	2026-11-01 10:19:51.99433+00	2026-12-01 10:19:51.992236+00	password	2026-11-01 10:34:51.992236+00	127.0.0.1	ureq/3.5.0
\\xd67f42146a6f21ec25885987681b0b55864adf5b72cf30cb0b71a1831165696c	01a1969f-68df-75b9-8c8e-4ceb31bd12a0	2026-11-01 10:19:52.133881+00	2026-11-01 10:19:52.133881+00	2026-12-01 10:19:52.132309+00	password	2026-11-01 10:34:52.132309+00	127.0.0.1	ureq/3.5.0
\\xd48555e46a54c42a97bb11269fb38a0eb5b21f8e6b78b079dda786db377ce210	01a1969f-68df-75b9-8c8e-4ceb31bd12a0	2026-11-01 10:19:53.014483+00	2026-11-01 10:19:53.014483+00	2026-12-01 10:19:53.013222+00	password	2026-11-01 10:34:53.013222+00	127.0.0.1	ureq/3.5.0
\\x24520122ccb75f4840cf4033169325dea8804a4e5c1ca589201874f0369569ba	01a1969f-6af7-7466-a978-a89adfb6d7aa	2026-11-01 10:19:53.286507+00	2026-11-01 10:19:53.286507+00	2026-12-01 10:19:53.285974+00	password	2026-11-01 10:34:53.285974+00	127.0.0.1	ureq/3.5.0
\\xc9afb2927f3e59f9a671f31fcb949537c8cfca7ecbc8b494f93b667aa1e6dee3	01a1969f-6af7-7466-a978-a89adfb6d7aa	2026-11-01 10:19:53.351765+00	2026-11-01 10:19:53.351765+00	2026-12-01 10:19:53.350871+00	password	2026-11-01 10:34:53.350871+00	127.0.0.1	ureq/3.5.0
\.


--
-- Data for Name: ssh_keys; Type: TABLE DATA; Schema: public; Owner: -
--

COPY public.ssh_keys (id, user_id, title, key_type, public_key, fingerprint_sha256, last_used_at, created_at) FROM stdin;
01a1969f-6e54-7150-95e8-d266aefd27b3	01a1969f-6af7-7466-a978-a89adfb6d7aa	test key	ssh-ed25519	AAAAC3NzaC1lZDI1NTE5AAAAIBKRv6jkJ4iUXbRX8VDPbs3ULyhFZrbP3hA9oVBHX4zg	SHA256:fh4ATFvLFM+W1TfSnry72WccMBo74BXeLohXb6E5SBI	\N	2026-11-01 10:19:53.301236+00
\.


--
-- Data for Name: users; Type: TABLE DATA; Schema: public; Owner: -
--

COPY public.users (id, namespace_id, kind, bot_owner_kind, bot_owner_id, display_name, email, email_verified, password_hash, is_admin, state, mfa_required, avatar_blob_sha256, timezone, theme, diff_view, editor_mode, created_at, updated_at, density, code_font_size) FROM stdin;
01a1969f-68df-75b9-8c8e-4ceb31bd12a0	01a1969f-68df-75b9-8c8e-4cea53f90da3	human	\N	\N	\N	alice@example.org	f	$argon2id$v=19$m=19456,t=2,p=1$bUpH+td348X+6K1fOaMUYQ$oGE+Zwgp8spJ+J/AG6RH4DV5/ThTGOsWGUotuFAMxQE	f	active	f	\N	\N	system	unified	wysiwyg	2026-11-01 10:19:51.900285+00	2026-11-01 10:19:51.900285+00	comfortable	14
01a1969f-6af7-7466-a978-a89adfb6d7aa	01a1969f-6af7-7466-a978-a89988510aa5	human	\N	\N	\N	bob@example.org	f	$argon2id$v=19$m=19456,t=2,p=1$J6KE2YVEz+pTIVYscgq9VA$GJTdGQcpZC7S3ALHztneJbxPHOg8noBxQaTat6Z3Q6I	f	active	f	\N	\N	system	unified	wysiwyg	2026-11-01 10:19:52.436164+00	2026-11-01 10:19:52.436164+00	comfortable	14
01a1969f-6c0b-7411-bf37-b9b5e549293b	01a1969f-6c0b-7411-bf37-b9b4b3b84f1e	human	\N	\N	\N	root-admin@example.org	f	$argon2id$v=19$m=19456,t=2,p=1$qYek1E5tnuQNTryYXaddWQ$w4ERTnIR6anNIwW/CCPalN91xHFChK7TmsmrA+LGktM	t	active	f	\N	\N	system	unified	wysiwyg	2026-11-01 10:19:52.711979+00	2026-11-01 10:19:52.711979+00	comfortable	14
\.


--
-- Name: jobs_id_seq; Type: SEQUENCE SET; Schema: public; Owner: -
--

SELECT pg_catalog.setval('public.jobs_id_seq', 1, true);


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
-- Name: audit_events audit_events_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.audit_events
    ADD CONSTRAINT audit_events_pkey PRIMARY KEY (id);


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
-- Name: instance_settings instance_settings_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.instance_settings
    ADD CONSTRAINT instance_settings_pkey PRIMARY KEY (key);


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
-- Name: login_attempts login_attempts_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.login_attempts
    ADD CONSTRAINT login_attempts_pkey PRIMARY KEY (key, window_start);


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
-- Name: push_events push_events_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.push_events
    ADD CONSTRAINT push_events_pkey PRIMARY KEY (id);


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
-- Name: repo_redirects repo_redirects_pkey; Type: CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.repo_redirects
    ADD CONSTRAINT repo_redirects_pkey PRIMARY KEY (namespace_id, name);


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
-- Name: access_tokens_user_id_idx; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX access_tokens_user_id_idx ON public.access_tokens USING btree (user_id);


--
-- Name: audit_events_created_at_idx; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX audit_events_created_at_idx ON public.audit_events USING btree (created_at);


--
-- Name: idempotency_keys_created_at_idx; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX idempotency_keys_created_at_idx ON public.idempotency_keys USING btree (created_at);


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
-- Name: org_members_user_id_idx; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX org_members_user_id_idx ON public.org_members USING btree (user_id);


--
-- Name: push_events_repo_id_idx; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX push_events_repo_id_idx ON public.push_events USING btree (repo_id, created_at DESC);


--
-- Name: repo_collaborators_user_id_idx; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX repo_collaborators_user_id_idx ON public.repo_collaborators USING btree (user_id);


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
-- Name: repositories_public_idx; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX repositories_public_idx ON public.repositories USING btree (updated_at DESC) WHERE ((visibility = 'public'::text) AND (state <> 'deleting'::text));


--
-- Name: sessions_expires_at_idx; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX sessions_expires_at_idx ON public.sessions USING btree (expires_at);


--
-- Name: sessions_user_id_idx; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX sessions_user_id_idx ON public.sessions USING btree (user_id);


--
-- Name: ssh_keys_user_id_idx; Type: INDEX; Schema: public; Owner: -
--

CREATE INDEX ssh_keys_user_id_idx ON public.ssh_keys USING btree (user_id);


--
-- Name: users_verified_email_key; Type: INDEX; Schema: public; Owner: -
--

CREATE UNIQUE INDEX users_verified_email_key ON public.users USING btree (email) WHERE email_verified;


--
-- Name: audit_events audit_events_append_only; Type: TRIGGER; Schema: public; Owner: -
--

CREATE TRIGGER audit_events_append_only BEFORE DELETE OR UPDATE ON public.audit_events FOR EACH ROW EXECUTE FUNCTION public.audit_events_append_only();


--
-- Name: access_tokens access_tokens_user_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.access_tokens
    ADD CONSTRAINT access_tokens_user_id_fkey FOREIGN KEY (user_id) REFERENCES public.users(id) ON DELETE CASCADE;


--
-- Name: git_refs git_refs_repo_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.git_refs
    ADD CONSTRAINT git_refs_repo_id_fkey FOREIGN KEY (repo_id) REFERENCES public.repositories(id) ON DELETE CASCADE;


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
-- Name: organizations organizations_namespace_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.organizations
    ADD CONSTRAINT organizations_namespace_id_fkey FOREIGN KEY (namespace_id) REFERENCES public.namespaces(id);


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
-- Name: sessions sessions_user_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.sessions
    ADD CONSTRAINT sessions_user_id_fkey FOREIGN KEY (user_id) REFERENCES public.users(id) ON DELETE CASCADE;


--
-- Name: ssh_keys ssh_keys_user_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.ssh_keys
    ADD CONSTRAINT ssh_keys_user_id_fkey FOREIGN KEY (user_id) REFERENCES public.users(id) ON DELETE CASCADE;


--
-- Name: users users_namespace_id_fkey; Type: FK CONSTRAINT; Schema: public; Owner: -
--

ALTER TABLE ONLY public.users
    ADD CONSTRAINT users_namespace_id_fkey FOREIGN KEY (namespace_id) REFERENCES public.namespaces(id);


--
-- PostgreSQL database dump complete
--

\unrestrict PqnvGyuZ73eZ59TmLUQcPsEjF0Ye5TbyOKh7NIl4AHh4hmbFQv1rndNanMs4r3V

