# Changelog

All notable changes to Slath are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and the project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).
Before 1.0.0, minor versions may contain breaking changes.

## [Unreleased]

### Added

- Cargo workspace (edition 2024) with the `core`, `db`, `git`, `markup`, `auth`,
  `service`, `http`, and `ci` crates and the `slath` binary, workspace lints
  (`unsafe_code` forbidden, `unwrap`/`expect` denied outside tests), and a
  `justfile` whose `just check` runs formatting, Clippy, and tests.
- License policy: license texts under `LICENSES/`, a `cargo deny`
  configuration (license allowlist, advisories, bans, sources), and
  `scripts/check_license_deps.py`, which fails when an Apache-2.0 workspace
  crate depends on an AGPL crate.
- `slath-core` foundations: UUIDv7 ID newtypes, user/org/repo name validation
  with reserved names, the redacting `Secret<T>` wrapper, authorization
  types (`Subject`, `Action`, `Role`, `ResourceCtx`), and a deny-all
  `authz::check` stub with a table-driven matrix test.
- `slath-git`: the hardened Git runner (scrubbed environment, fixed config,
  process-group kill on timeout/output cap/drop, concurrency semaphore,
  a minimum Git version check that, before 1.0, requires the newest stable
  Git release), validators for user revisions, ref names, and paths, an
  object reader over `git cat-file --batch-command`, and a raw commit parser
  that round-trips commits byte for byte and exposes jj `change-id`.
- CI workflow running `just check`, an MSRV build, and `scripts/guards.sh`,
  which enforces SPDX headers, Git spawns only from the runner, no shells in
  server code, no Askama `|safe`, and no third-party URLs in resource
  positions. `scripts/guards_test.sh` checks that each guard fires.
- Developer environment: `deploy/dev/compose.yml` (PostgreSQL 18),
  `just setup`, `just dev-db`, `.env.example`, and `docs/dev/setup.md`.
- `slath-db`: connection pool, embedded migrations with a schema version check,
  the first migration (`namespaces`, `users`, `reserved_names`, `jobs`,
  `audit_events`, `instance_settings`), and the durable job queue with dedupe
  keys, `SKIP LOCKED` leases, heartbeats, exponential backoff, a `dead` state,
  `LISTEN/NOTIFY` wake-ups, and a worker pool.
- Deterministic fixture generators (`tests/fixtures/make_*.sh`: basic, unicode,
  binary/symlink/exec, merges, tags, signed, submodule, deep history) whose
  refs and OIDs are identical on every run and across Git versions, plus
  `scripts/install-git.sh` and `scripts/install-jj.sh`, which install
  SHA-256-pinned versions from the `tests/compat/versions.toml` matrix.
- Conformance harness: a nextest `ci` profile with JUnit output and
  `just conformance-report CASES [--gate <milestone>]`, which maps `conf_<area>_<nn>_*`
  tests to conformance cases and writes `target/conformance/report.{json,md}`.
- `slath serve` (transport spike) serves Git smart HTTP for repositories
  listed in the config, with protocol v0/v2, gzip request bodies, and a
  static token. Pushes go through managed `pre-receive`/`post-receive` hooks
  (`slath hook`), which reach the server over a 0600 Unix socket, deny
  `refs/slath/**` and `refs/merge-requests/**`, and fail closed when the
  server can't be reached.
- Git over SSH through OpenSSH: `slath ssh-keys` (the `AuthorizedKeysCommand`)
  and `slath ssh-serve` (the forced command) ask `slath serve` over the IPC
  socket and allow only `git-upload-pack`/`git-receive-pack` on repositories
  the key may use. Shells, other commands, `git-upload-archive`, path tricks,
  TTYs, and forwarding are refused. `deploy/sshd/slath.conf` is the sshd
  drop-in, and the SSH tests run against a containerized sshd that uses it.
- Jujutsu compatibility tests: `jj git clone` and `jj git push` over HTTPS and
  SSH for every pinned jj version, with a second jj and a plain Git clone
  seeing the same commits and change IDs. They also record how jj stores
  change IDs and conflicts in Git commits. `just tools` installs the pinned
  jj versions.
- README with the project's purpose, status, and development quick start.
- Configuration loading: a TOML file overridden by `SLATH__<SECTION>__<KEY>`
  environment variables, typed sections with defaults, every limit under
  `[limits]`, durations (`30s`, `1h`) and sizes (`10MiB`), the database URL
  from `url` or `url_file`, and errors that name unknown or invalid keys.
- `slath config check` (Git version, writable `data_dir`, database
  connectivity and schema, master key permissions), `slath version [--json]`,
  `slath migrate [--dry-run]`, and `slath admin gen-key`. Administrative
  commands accept `--output json`.
- HTTP server foundations: request IDs (`X-Request-Id`), structured access
  logs, Prometheus metrics on a separate listener, a request body limit,
  security headers on every response with a fresh CSP nonce per response,
  RFC 9457 problem JSON for API errors and HTML error pages for the UI,
  `/-/healthz`, `/-/readyz` (database, schema, `data_dir`, job workers), and
  `/-/csp-report`. `serve` now needs the database and accepts `--migrate`.
- UI shell: Askama templates with a base layout (landmarks, skip link,
  page titles), the UI bundle built from `web/` with Vite (htmx 4 with the
  `hx-csp` extension, CSRF header, accessible swaps, error toasts) and
  embedded in the binary under content-hashed URLs, and a development-only
  component catalog at `/-/design`. A guard rejects htmx elements without the
  nonce macro and eval-based htmx features.
- Browser tests (`just test-e2e`): Playwright in Chromium, Firefox, and
  WebKit, each in light and dark themes on desktop and touch viewports, with
  axe-core accessibility checks and failures on third-party requests, CSP
  violations, and stripped htmx elements.
- Weave design tokens (`web/src/tokens.json` → `web/src/tokens.css`):
  light, dark, and high-contrast color sets (following
  `prefers-color-scheme`, `prefers-contrast`, and a server-rendered override),
  semantic success/warning/error/info colors, diff tints with word-level
  emphasis, syntax colors, typography, spacing, radii, elevation, motion, and
  density. `just tokens-check` (part of `just check`) verifies every declared
  color pair's contrast and that semantic colors stay distinct from the
  accent.
- Bundled fonts and icons: Source Sans 3 (Adobe's variable WOFF2, unmodified)
  and JetBrains Mono (subset), both OFL-1.1, with metric-matched fallback
  faces so font loading doesn't shift the layout, the regular face
  preloaded, and ligatures off in code. A Lucide (ISC) icon sprite holds only
  the allowlisted icons. `scripts/fonts.py` rebuilds the fonts from pinned,
  SHA-256-checked releases. `THIRD-PARTY-NOTICES.md` lists every bundled
  component, and `just check` fails if one is missing.
