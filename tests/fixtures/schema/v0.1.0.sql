-- SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
-- SPDX-License-Identifier: AGPL-3.0-or-later
--
-- Schema and data of Slath 0.1.0 (schema 9), written by `just schema-fixture`.
-- Users: alice, bob, root-admin (password "slath test password"). Don't edit.

--
-- PostgreSQL database dump
--

\restrict 1stmCk1ydOydRoSSFvXfvqN3Pift1cMI6KaWDuHoCKmfP69DcEQ5Yw2wM1bS49D

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
1	foundation	2026-11-01 08:09:25.531937+00	t	\\xd33ee3413e24792c0944e0147487310b878147391c3f1f8d058cfbdd50ac3f26cc9d362f61b0b5080e49cbf9674f8a6a	36158959
2	sessions	2026-11-01 08:09:25.569588+00	t	\\xbb6c166b89823c9d6f520c51e35d4a822ea6fcb61b8867110c6edac424e46bac6facac324dc52834a169dd98d4aaba49	6906758
3	access tokens	2026-11-01 08:09:25.577136+00	t	\\xf301152e64bc266d37146a892bd84566c5325f462f76cb396b09465221140ad905e2891d6cfcaede2a57a91a405ab4c7	5473711
4	ssh keys	2026-11-01 08:09:25.583237+00	t	\\xa38cbfa2b5f8449425974aac945ec6d726dff155bf1894350dac861aec1c5cb1079456c5dd30ce4054ed60d5a002a3d7	5277350
5	organizations	2026-11-01 08:09:25.588901+00	t	\\xce9a9b09ac147403c62660cabdb029bded5c22463e6f116a0c0a7dc301dcd411216d86a46c9cdcc4ff8d22741fb05612	7177128
6	repositories	2026-11-01 08:09:25.596634+00	t	\\x2d45e0f09a670784913372e54599c58495a5e6e5262ef59024b7cd3528b546d07052117cc66f0133476495ecc47ad927	20274661
7	idempotency keys	2026-11-01 08:09:25.617841+00	t	\\x176f61cb4640020001d7ae19cc9b038f70d37fe5a0e94f1cf9b1ed9a25b632c340a6d6fa77c850a588d3151164b66c2b	4331352
8	repo maintenance	2026-11-01 08:09:25.622448+00	t	\\x59525f963724f9333c4d62586341526d0497f399e92ba70d7ebb457dc0cef0e664056fbd93523401aaf29445e37df6f1	3474985
9	appearance	2026-11-01 08:09:25.626375+00	t	\\x28a35cb29bd16eaf97d79bc19e4f04230f4bfad09d631dfa87aa105eba1d1f2a1d4f837c4f41dda3d6f25b381be1b991	2869401
\.


--
-- Data for Name: access_tokens; Type: TABLE DATA; Schema: public; Owner: -
--

COPY public.access_tokens (id, user_id, name, token_prefix, token_hash, scopes, repo_ids, expires_at, last_used_at, revoked_at, created_at) FROM stdin;
01a19627-ffeb-7627-af34-4fff3a70d4ba	01a19627-fee9-7350-a5f8-b78c79b4cef5	tests	70siw3qZ	\\x4a71d839e45865735726536631aacee1d7d56eaaba29add5202f533bc94440f7	{repo:write,user:read}	\N	2026-12-01 08:09:26.251476+00	2026-11-01 08:09:26.914227+00	\N	2026-11-01 08:09:26.253558+00
01a19628-047b-7727-b44c-bd2aba8ba05f	01a19628-00fa-7671-b347-103c18c4c68e	ci	0SimMQYt	\\x130d6b61310ebd41e08dd392c1097d0f6446ae19c101ccb976453ac499d88a8d	{repo:read}	\N	2026-12-01 08:09:27.419981+00	\N	\N	2026-11-01 08:09:27.4204+00
\.


--
-- Data for Name: audit_events; Type: TABLE DATA; Schema: public; Owner: -
--

COPY public.audit_events (id, actor_id, actor_ip, action, target_kind, target_id, repo_id, org_id, data, created_at) FROM stdin;
01a19627-feed-74b7-a900-82caa1e5c1b4	\N	\N	user.create	user	01a19627-fee9-7350-a5f8-b78c79b4cef5	\N	\N	{"admin": false, "username": "alice"}	2026-11-01 08:09:25.999833+00
01a19627-ff42-71f5-9405-6863425d35d2	01a19627-fee9-7350-a5f8-b78c79b4cef5	127.0.0.1	login.success	user	01a19627-fee9-7350-a5f8-b78c79b4cef5	\N	\N	{}	2026-11-01 08:09:26.083246+00
01a19627-ff8b-7468-9891-b7b0e993783a	01a19627-fee9-7350-a5f8-b78c79b4cef5	127.0.0.1	repo.create	repository	01a19627-ff58-7263-bed5-603ece99e1d3	01a19627-ff58-7263-bed5-603ece99e1d3	\N	{"name": "alice/demo", "visibility": "private"}	2026-11-01 08:09:26.156005+00
01a19627-ffd8-75fc-a597-07a20f5908a2	01a19627-fee9-7350-a5f8-b78c79b4cef5	127.0.0.1	login.success	user	01a19627-fee9-7350-a5f8-b78c79b4cef5	\N	\N	{}	2026-11-01 08:09:26.234578+00
01a19627-ffef-725a-922c-1954a58cf526	01a19627-fee9-7350-a5f8-b78c79b4cef5	127.0.0.1	token.create	token	01a19627-ffeb-7627-af34-4fff3a70d4ba	\N	\N	{"name": "tests", "scopes": ["repo:write", "user:read"], "expires_in_days": 30}	2026-11-01 08:09:26.256661+00
01a19628-0100-7215-80fc-0982d405183a	\N	\N	user.create	user	01a19628-00fa-7671-b347-103c18c4c68e	\N	\N	{"admin": false, "username": "bob"}	2026-11-01 08:09:26.529272+00
01a19628-0212-70d0-a152-ec2aaeeb4edc	\N	\N	user.create	user	01a19628-020e-720f-93c6-70bc1226fe32	\N	\N	{"admin": true, "username": "root-admin"}	2026-11-01 08:09:26.803949+00
01a19628-0321-7524-87d1-84d53a77c842	01a19627-fee9-7350-a5f8-b78c79b4cef5	127.0.0.1	login.success	user	01a19627-fee9-7350-a5f8-b78c79b4cef5	\N	\N	{}	2026-11-01 08:09:27.074991+00
01a19628-0330-749f-850f-08479823c8e8	01a19627-fee9-7350-a5f8-b78c79b4cef5	127.0.0.1	org.create	organization	01a19628-032c-7300-9f32-f7477cd4bf58	\N	01a19628-032c-7300-9f32-f7477cd4bf58	{"name": "acme"}	2026-11-01 08:09:27.089257+00
01a19628-0343-74f1-a016-5d0397b4d1e1	01a19627-fee9-7350-a5f8-b78c79b4cef5	127.0.0.1	org.member.set	organization	01a19628-032c-7300-9f32-f7477cd4bf58	\N	01a19628-032c-7300-9f32-f7477cd4bf58	{"role": "member", "username": "bob"}	2026-11-01 08:09:27.108121+00
01a19628-0387-771f-83c6-889e0687b5c7	01a19627-fee9-7350-a5f8-b78c79b4cef5	127.0.0.1	repo.create	repository	01a19628-034f-7523-80c6-7db1e85e4702	01a19628-034f-7523-80c6-7db1e85e4702	01a19628-032c-7300-9f32-f7477cd4bf58	{"name": "acme/tools", "visibility": "public"}	2026-11-01 08:09:27.176047+00
01a19628-0399-718e-abfd-3c860fa8e53d	01a19627-fee9-7350-a5f8-b78c79b4cef5	127.0.0.1	repo.collaborator.set	repository	01a19627-ff58-7263-bed5-603ece99e1d3	01a19627-ff58-7263-bed5-603ece99e1d3	\N	{"role": "write", "username": "bob"}	2026-11-01 08:09:27.194157+00
01a19628-03d5-7527-b609-eda8e934ab4b	01a19627-fee9-7350-a5f8-b78c79b4cef5	127.0.0.1	repo.create	repository	01a19628-03a5-7089-8983-6c9297452c50	01a19628-03a5-7089-8983-6c9297452c50	\N	{"name": "alice/renamed", "visibility": "private"}	2026-11-01 08:09:27.254415+00
01a19628-03ee-771f-a730-23e0aac7aa0d	01a19627-fee9-7350-a5f8-b78c79b4cef5	127.0.0.1	repo.rename	repository	01a19628-03a5-7089-8983-6c9297452c50	01a19628-03a5-7089-8983-6c9297452c50	\N	{"to": "new-name", "from": "renamed"}	2026-11-01 08:09:27.278818+00
01a19628-0430-7503-89f6-e31ddc47f4af	01a19628-00fa-7671-b347-103c18c4c68e	127.0.0.1	login.success	user	01a19628-00fa-7671-b347-103c18c4c68e	\N	\N	{}	2026-11-01 08:09:27.345458+00
01a19628-0440-7173-b8a7-e0a2655b3489	01a19628-00fa-7671-b347-103c18c4c68e	127.0.0.1	ssh_key.add	ssh_key	01a19628-043c-77e9-b937-7c893ecc3d7d	\N	\N	{"title": "test key", "fingerprint": "SHA256:muttnDqpRyeB+TiFklihkz5jIAcKdMd4KhCXiu8Yj7c"}	2026-11-01 08:09:27.360941+00
01a19628-0472-769a-84b9-7805634994cb	01a19628-00fa-7671-b347-103c18c4c68e	127.0.0.1	login.success	user	01a19628-00fa-7671-b347-103c18c4c68e	\N	\N	{}	2026-11-01 08:09:27.411667+00
01a19628-047d-740f-a750-ba03e263b9d5	01a19628-00fa-7671-b347-103c18c4c68e	127.0.0.1	token.create	token	01a19628-047b-7727-b44c-bd2aba8ba05f	\N	\N	{"name": "ci", "scopes": ["repo:read"], "expires_in_days": 30}	2026-11-01 08:09:27.421481+00
\.


--
-- Data for Name: git_refs; Type: TABLE DATA; Schema: public; Owner: -
--

COPY public.git_refs (repo_id, name, oid, updated_at) FROM stdin;
01a19627-ff58-7263-bed5-603ece99e1d3	\\x726566732f68656164732f6d61696e	e31a2bdcfa4366d63eec483681f2e7bef2528dd9	2026-11-01 08:09:26.993158+00
01a19627-ff58-7263-bed5-603ece99e1d3	\\x726566732f746167732f72656c656173652f322e30	822b6c5fab6d985b4dbf8caccf6ca5db811ab916	2026-11-01 08:09:26.993158+00
01a19627-ff58-7263-bed5-603ece99e1d3	\\x726566732f746167732f747265652d746167	b952f8c74fe4b4f428492d75c1e227657be2bf48	2026-11-01 08:09:26.993158+00
01a19627-ff58-7263-bed5-603ece99e1d3	\\x726566732f746167732f76312e30	032aee9a437815204df8a21bbc7371dcec2bd0e3	2026-11-01 08:09:26.993158+00
01a19627-ff58-7263-bed5-603ece99e1d3	\\x726566732f746167732f76312e302d6c69676874	3f3ec41f7a4fefa97602911c0258528cc2ad3d25	2026-11-01 08:09:26.993158+00
01a19627-ff58-7263-bed5-603ece99e1d3	\\x726566732f746167732f76322e302d6d657461	e1e0b1fbb57d0584f431150d048088a9e1508dbe	2026-11-01 08:09:26.993158+00
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
1	default	ref.updated	{"push_id": "01a19628-02d1-7381-ac22-c57a51b587b9", "repo_id": "01a19627-ff58-7263-bed5-603ece99e1d3"}	ref.updated:01a19628-02d1-7381-ac22-c57a51b587b9	done	2026-11-01 08:09:26.993158+00	1	8	\N	\N	\N	2026-11-01 08:09:26.993158+00	2026-11-01 08:09:27.013151+00
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
01a19627-fee9-7350-a5f8-b78b1a9581e6	alice	user	2026-11-01 08:09:25.990775+00
01a19628-00fa-7671-b347-103b0d30c176	bob	user	2026-11-01 08:09:26.518317+00
01a19628-020e-720f-93c6-70bb06bddb36	root-admin	user	2026-11-01 08:09:26.796349+00
01a19628-032c-7300-9f32-f746ddc8df2a	acme	org	2026-11-01 08:09:27.083625+00
\.


--
-- Data for Name: org_members; Type: TABLE DATA; Schema: public; Owner: -
--

COPY public.org_members (org_id, user_id, role, created_at) FROM stdin;
01a19628-032c-7300-9f32-f7477cd4bf58	01a19627-fee9-7350-a5f8-b78c79b4cef5	owner	2026-11-01 08:09:27.083625+00
01a19628-032c-7300-9f32-f7477cd4bf58	01a19628-00fa-7671-b347-103c18c4c68e	member	2026-11-01 08:09:27.105391+00
\.


--
-- Data for Name: organizations; Type: TABLE DATA; Schema: public; Owner: -
--

COPY public.organizations (id, namespace_id, display_name, description, default_member_role, created_at, updated_at) FROM stdin;
01a19628-032c-7300-9f32-f7477cd4bf58	01a19628-032c-7300-9f32-f746ddc8df2a	Acme	\N	none	2026-11-01 08:09:27.083625+00	2026-11-01 08:09:27.083625+00
\.


--
-- Data for Name: push_events; Type: TABLE DATA; Schema: public; Owner: -
--

COPY public.push_events (id, repo_id, pusher_id, transport, created_at) FROM stdin;
01a19628-02d1-7381-ac22-c57a51b587b9	01a19627-ff58-7263-bed5-603ece99e1d3	01a19627-fee9-7350-a5f8-b78c79b4cef5	http	2026-11-01 08:09:26.993158+00
\.


--
-- Data for Name: ref_updates; Type: TABLE DATA; Schema: public; Owner: -
--

COPY public.ref_updates (push_id, seq, name, old_oid, new_oid) FROM stdin;
01a19628-02d1-7381-ac22-c57a51b587b9	0	\\x726566732f68656164732f6d61696e	\N	e31a2bdcfa4366d63eec483681f2e7bef2528dd9
01a19628-02d1-7381-ac22-c57a51b587b9	1	\\x726566732f746167732f72656c656173652f322e30	\N	822b6c5fab6d985b4dbf8caccf6ca5db811ab916
01a19628-02d1-7381-ac22-c57a51b587b9	2	\\x726566732f746167732f747265652d746167	\N	b952f8c74fe4b4f428492d75c1e227657be2bf48
01a19628-02d1-7381-ac22-c57a51b587b9	3	\\x726566732f746167732f76312e30	\N	032aee9a437815204df8a21bbc7371dcec2bd0e3
01a19628-02d1-7381-ac22-c57a51b587b9	4	\\x726566732f746167732f76312e302d6c69676874	\N	3f3ec41f7a4fefa97602911c0258528cc2ad3d25
01a19628-02d1-7381-ac22-c57a51b587b9	5	\\x726566732f746167732f76322e302d6d657461	\N	e1e0b1fbb57d0584f431150d048088a9e1508dbe
\.


--
-- Data for Name: repo_collaborators; Type: TABLE DATA; Schema: public; Owner: -
--

COPY public.repo_collaborators (repo_id, user_id, role, created_at) FROM stdin;
01a19628-034f-7523-80c6-7db1e85e4702	01a19627-fee9-7350-a5f8-b78c79b4cef5	admin	2026-11-01 08:09:27.120422+00
01a19627-ff58-7263-bed5-603ece99e1d3	01a19628-00fa-7671-b347-103c18c4c68e	write	2026-11-01 08:09:27.192713+00
\.


--
-- Data for Name: repo_redirects; Type: TABLE DATA; Schema: public; Owner: -
--

COPY public.repo_redirects (namespace_id, name, repo_id, created_at) FROM stdin;
01a19627-fee9-7350-a5f8-b78b1a9581e6	renamed	01a19628-03a5-7089-8983-6c9297452c50	2026-11-01 08:09:27.269861+00
\.


--
-- Data for Name: repositories; Type: TABLE DATA; Schema: public; Owner: -
--

COPY public.repositories (id, namespace_id, name, description, visibility, default_branch, state, fork_parent_id, fork_network_id, allow_forking, trust_bot_runs, auto_approve_external_for, issues_enabled, merge_requests_enabled, size_bytes, next_item_number, merge_strategies, default_merge_strategy, delete_source_branch_default, archived_reason, deleted_at, pushed_at, created_at, updated_at, pushes_since_maintenance, maintained_at, maintenance_runs, maintenance_lease_until) FROM stdin;
01a19627-ff58-7263-bed5-603ece99e1d3	01a19627-fee9-7350-a5f8-b78b1a9581e6	demo	\N	private	main	active	\N	01a19627-ff58-7263-bed5-603ece99e1d3	t	f	none	t	t	40960	1	{fast-forward,rebase,merge-commit,squash}	rebase	t	\N	\N	2026-11-01 08:09:26.993158+00	2026-11-01 08:09:26.105811+00	2026-11-01 08:09:26.105811+00	1	\N	0	\N
01a19628-034f-7523-80c6-7db1e85e4702	01a19628-032c-7300-9f32-f746ddc8df2a	tools	\N	public	main	active	\N	01a19628-034f-7523-80c6-7db1e85e4702	t	f	none	t	t	0	1	{fast-forward,rebase,merge-commit,squash}	rebase	t	\N	\N	\N	2026-11-01 08:09:27.120422+00	2026-11-01 08:09:27.120422+00	0	\N	0	\N
01a19628-03a5-7089-8983-6c9297452c50	01a19627-fee9-7350-a5f8-b78b1a9581e6	new-name	\N	private	main	active	\N	01a19628-03a5-7089-8983-6c9297452c50	t	f	none	t	t	0	1	{fast-forward,rebase,merge-commit,squash}	rebase	t	\N	\N	\N	2026-11-01 08:09:27.206288+00	2026-11-01 08:09:27.269861+00	0	\N	0	\N
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
\\x281bf28c891480bb46bd389e313db8ba8b9432fac978440ecea3832aef17ddb3	01a19627-fee9-7350-a5f8-b78c79b4cef5	2026-11-01 08:09:26.080897+00	2026-11-01 08:09:26.080897+00	2026-12-01 08:09:26.078531+00	password	2026-11-01 08:24:26.078531+00	127.0.0.1	ureq/3.5.0
\\x7f5cbb02c8dba5d946bda445320c22f3636c3189edd0527d88003830d32ca1e1	01a19627-fee9-7350-a5f8-b78c79b4cef5	2026-11-01 08:09:26.230065+00	2026-11-01 08:09:26.230065+00	2026-12-01 08:09:26.227749+00	password	2026-11-01 08:24:26.227749+00	127.0.0.1	ureq/3.5.0
\\xa578a3243dc876c41edab91143e7551d2ff2fc45d4aa6a57f8fb00a66bc1aeb0	01a19627-fee9-7350-a5f8-b78c79b4cef5	2026-11-01 08:09:27.072771+00	2026-11-01 08:09:27.072771+00	2026-12-01 08:09:27.071996+00	password	2026-11-01 08:24:27.071996+00	127.0.0.1	ureq/3.5.0
\\x6df72521c362f4d5b233287f2418c6cd1327dd3f947e849289357c7a97218a58	01a19628-00fa-7671-b347-103c18c4c68e	2026-11-01 08:09:27.343649+00	2026-11-01 08:09:27.343649+00	2026-12-01 08:09:27.343091+00	password	2026-11-01 08:24:27.343091+00	127.0.0.1	ureq/3.5.0
\\x6156862b8722b196c3f64eca14da0cdd1bec650f8fc17de02ea09bd5c6cd7626	01a19628-00fa-7671-b347-103c18c4c68e	2026-11-01 08:09:27.409727+00	2026-11-01 08:09:27.409727+00	2026-12-01 08:09:27.409028+00	password	2026-11-01 08:24:27.409028+00	127.0.0.1	ureq/3.5.0
\.


--
-- Data for Name: ssh_keys; Type: TABLE DATA; Schema: public; Owner: -
--

COPY public.ssh_keys (id, user_id, title, key_type, public_key, fingerprint_sha256, last_used_at, created_at) FROM stdin;
01a19628-043c-77e9-b937-7c893ecc3d7d	01a19628-00fa-7671-b347-103c18c4c68e	test key	ssh-ed25519	AAAAC3NzaC1lZDI1NTE5AAAAIEZ/820SvPAoe84LI3+xB89amLKtbMewYc6bdCPVhRpP	SHA256:muttnDqpRyeB+TiFklihkz5jIAcKdMd4KhCXiu8Yj7c	\N	2026-11-01 08:09:27.358094+00
\.


--
-- Data for Name: users; Type: TABLE DATA; Schema: public; Owner: -
--

COPY public.users (id, namespace_id, kind, bot_owner_kind, bot_owner_id, display_name, email, email_verified, password_hash, is_admin, state, mfa_required, avatar_blob_sha256, timezone, theme, diff_view, editor_mode, created_at, updated_at, density, code_font_size) FROM stdin;
01a19627-fee9-7350-a5f8-b78c79b4cef5	01a19627-fee9-7350-a5f8-b78b1a9581e6	human	\N	\N	\N	alice@example.org	f	$argon2id$v=19$m=19456,t=2,p=1$eQ7M91zaHS7iO9vmCJWXjw$an6F5HFcjOlqz41Not/qwfz00wWKoU091C6P5sG7fLk	f	active	f	\N	\N	system	unified	wysiwyg	2026-11-01 08:09:25.990775+00	2026-11-01 08:09:25.990775+00	comfortable	14
01a19628-00fa-7671-b347-103c18c4c68e	01a19628-00fa-7671-b347-103b0d30c176	human	\N	\N	\N	bob@example.org	f	$argon2id$v=19$m=19456,t=2,p=1$LbhbXjkcLpZ60LiEZlskzA$9qXDKYpc0uJdgiCJ47GAdt/LFt3tqoNwEsuC4IGlcKA	f	active	f	\N	\N	system	unified	wysiwyg	2026-11-01 08:09:26.518317+00	2026-11-01 08:09:26.518317+00	comfortable	14
01a19628-020e-720f-93c6-70bc1226fe32	01a19628-020e-720f-93c6-70bb06bddb36	human	\N	\N	\N	root-admin@example.org	f	$argon2id$v=19$m=19456,t=2,p=1$vEo9gisKK/U7wSLqpuLy4w$nYS296tAIzeMnZmUzMRE4OTFbXupEs93af6tKermZGE	t	active	f	\N	\N	system	unified	wysiwyg	2026-11-01 08:09:26.796349+00	2026-11-01 08:09:26.796349+00	comfortable	14
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

\unrestrict 1stmCk1ydOydRoSSFvXfvqN3Pift1cMI6KaWDuHoCKmfP69DcEQ5Yw2wM1bS49D

