# Changelog

All notable changes to Slath are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and the project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).
Before 1.0.0, minor versions may contain breaking changes.

## [Unreleased]

### Changed

- Administrators need two-factor authentication to use administration unless `require_mfa_for_admins` is turned off; see the upgrading guide.

### Added

- Secrets that Slath must read again are encrypted with AES-256-GCM under the master key; each value is bound to its row.
- `slath admin rotate-key` re-encrypts stored secrets with a new master key and retires old keys once nothing uses them.
- `slath config migrate` rewrites renamed configuration keys and keeps comments; old keys keep working with a warning.
- `slath upgrade` upgrades from any earlier release in one command: preflight checks, an automatic backup, migrations, verification, and a history.
- `slath upgrade --check` shows the plan and the upgrade notes of every skipped release without changing anything.
- During an upgrade a running server becomes read-only: pages show a banner, and saving and pushing are refused with a message.
- `serve --migrate` runs the same preflight and refuses to migrate when a check fails or a manual upgrade step is required.
- An older binary on an upgraded database names the upgrade and the backup to restore.
- Branch protection: block force pushes and deletion, limit who may push directly, and require merge requests with an audited admin bypass.
- Teams: organization owners group members into teams, and repository administrators give teams a role.
- Organizations can require two-factor authentication for access to their repositories and give every member read access.
- Registration modes: `disabled` (default), `invite` with single-use invitation links from administrators, and `open` self-registration.
- On open instances with email, accounts must confirm their address before creating repositories or organizations.
- Password reset by email: single-use links valid for 30 minutes, the same answer for unknown addresses, and every session signed out afterwards.
- Email addresses are confirmed by a link; users change theirs under Settings → Profile, and resets go only to confirmed addresses.
- Two-factor authentication with authenticator apps (TOTP) and single-use recovery codes, set up under Settings → Security.
- Passkeys: sign in without a password, use one as the second step, or confirm sensitive actions with it; managed under Settings → Security.
- Signing in and confirming sensitive actions ask for a two-factor code when it's set up; five wrong codes end the attempt.
- With `require_mfa_for_admins` (on by default), administration works only after signing in with a second factor.
- `slath admin reset-mfa` removes a user's second factors after a lost device; audited, and the user is notified by email.
- Outgoing email over SMTP with retries; `slath admin send-test-email` and the admin Health page check the settings.
- Without SMTP settings, email features are hidden.
- `slath serve` waits up to a minute for the database at startup instead of failing at once.
- Long data migrations run in the background in resumable batches; `slath admin background-migrations` and the admin page Upgrades show progress and pause, resume, or retry them.
- Every release ships machine-readable upgrade notes, and releases without them fail the release checks.

## [0.1.1] - 2026-11-01

Security hardening: the SSH login no longer shares the server's system user. See `docs/admin/upgrading.md` before upgrading a systemd installation.

### Security

- The SSH login `git` can be a separate system user that reaches only the server's socket; Git for SSH now runs in the server, so the login can't read repositories or secrets.
- The container image and the Compose setup separate the SSH login by default, and the sshd container no longer mounts the data volume.
- `slath config check` fails if the SSH login's group can read the data directory, the master key, or the database URL file.

### Added

- `storage.socket_group` lets the SSH login's group connect to the server's socket. Without it, only the server's own user can, as before.

### Changed

- The sshd drop-in runs `slath ssh-keys` as `git` instead of `slath`, and the systemd files create `git` as its own user. See `docs/admin/upgrading.md` for existing installations.
- The install guide shows how to trust only the reverse proxy's actual subnet instead of a broad private range.

## [0.1.0] - 2026-11-01

First release: self-hosted Git and Jujutsu hosting for small teams.

### Added

- Cargo workspace with the `core`, `db`, `git`, `markup`, `auth`, `service`, `http`, and `ci` crates, the `slath` binary, strict workspace lints, and `just check`.
- License texts under `LICENSES/`, a `cargo deny` policy, and a check that no Apache-2.0 crate depends on an AGPL crate.
- Core types: UUIDv7 IDs, name validation with reserved names, the redacting `Secret<T>`, and authorization types.
- Hardened Git runner: scrubbed environment, fixed config, timeouts, output caps, concurrency limit, and a check for the newest stable Git.
- Validators for revisions, ref names, and paths; an object reader over `git cat-file`; a byte-exact commit parser that exposes jj change IDs.
- CI running `just check`, an MSRV build, and `scripts/guards.sh`, which enforces SPDX headers, no shells, no `|safe`, and no third-party URLs.
- Developer environment: PostgreSQL 18 via `deploy/dev/compose.yml`, `just setup`, `just dev-db`, and `docs/dev/setup.md`.
- Database layer: connection pool, embedded migrations with a schema version check, and the first schema.
- Durable job queue with dedupe keys, `SKIP LOCKED` leases, heartbeats, exponential backoff, a dead state, and `LISTEN/NOTIFY` wake-ups.
- Deterministic Git fixture generators with identical OIDs on every run, and installers for SHA-256-pinned Git and jj versions.
- Conformance harness: JUnit output and `just conformance-report`, which maps `conf_*` tests to conformance cases.
- Git smart HTTP (protocol v0 and v2) with managed pre- and post-receive hooks that talk to the server over a 0600 Unix socket and fail closed.
- Git over SSH through OpenSSH: `slath ssh-keys` and `slath ssh-serve` allow only fetch and push on permitted repositories; shells and forwarding are refused.
- `deploy/sshd/slath.conf`, the sshd drop-in, tested against a containerized sshd.
- Jujutsu compatibility tests: `jj git clone` and `jj git push` over HTTPS and SSH for every pinned jj version.
- README with the project's purpose, status, and development quick start.
- TOML configuration with `SLATH__<SECTION>__<KEY>` overrides, typed defaults, all limits under `[limits]`, and errors naming unknown keys.
- `slath config check`, `slath version`, `slath migrate`, and `slath admin gen-key`; admin commands accept `--output json`.
- HTTP server basics: request IDs, access logs, Prometheus metrics on a separate listener, body limits, and security headers with a CSP nonce.
- RFC 9457 problem JSON for API errors, HTML error pages for the UI, and `/-/healthz`, `/-/readyz`, and `/-/csp-report`.
- UI shell: Askama layouts with landmarks and skip link, and a Vite bundle (htmx 4 with `hx-csp`) embedded under content-hashed URLs.
- Development-only component catalog at `/-/design`.
- Browser tests (`just test-e2e`) in Chromium, Firefox, and WebKit, light and dark, desktop and touch, with axe accessibility checks.
- Weave design tokens: light, dark, and high-contrast colors plus typography, spacing, radii, motion, and density, with contrast checks.
- Bundled Source Sans 3 and JetBrains Mono fonts (OFL-1.1) with metric-matched fallbacks, and a Lucide (ISC) icon sprite.
- `THIRD-PARTY-NOTICES.md` lists every bundled component; `just check` fails if one is missing.
- Local accounts: `slath admin create-user`, Argon2id password hashes, and a 12–1024 character password policy.
- Sign-in and sign-out with hashed session cookies, idle and absolute expiry, and throttling per account and per IP.
- CSRF protection for every state-changing browser request: same-origin checks plus a token.
- Account settings: display name, generated avatar, password change, and a sessions list with sign-out per session or everywhere.
- Sudo mode: sensitive actions ask for the password again, valid for 15 minutes.
- Personal access tokens with scopes and expiry, shown once and stored only as a hash.
- `/api/v1` accepts only `Authorization: Bearer` tokens and ignores cookies.
- SSH keys in account settings: Ed25519, ECDSA, security keys, and RSA of at least 3072 bits; weak or malformed keys are rejected with a reason.
- Organizations with owners and members, a protected last owner, and profile pages at `/<name>`.
- Users and organizations share one case-insensitive namespace.
- Repositories: create in your namespace or an organization, with clone URLs and push instructions on the empty-repository page.
- Repository settings: description, default branch, visibility, rename with redirects for readers, archive, and collaborators with roles.
- Repository deletion: invisible at once, then kept in the trash for `maintenance.deleted_repo_days` before it's purged.
- Authorization policy for every repository action, pinned by a 1444-cell matrix test; invisible resources answer "not found".
- Git over HTTPS and SSH serve repositories from the database; HTTPS uses tokens as passwords, and private and missing repos look the same.
- Push policy: internal refs are refused, pushes need write access, archived repos are read-only, and moving tags needs the maintain role.
- Accepted pushes are recorded as push events, ref updates, and a ref snapshot.
- Reconciler: `slath fsck --reconcile` and `slath serve` repair database refs that differ from Git.
- Markdown rendering with an allowlist sanitizer, CSS-class syntax highlighting, and visible text-direction controls; pinned by an XSS corpus.
- Code browser: files, README, highlighted code with line anchors, history, commits with diffs, and branch and tag lists.
- Raw files are served as text, verified images, or downloads, always with a sandbox CSP; symlinks are served as their target text.
- Archive downloads as `.tar.gz` and `.zip`, with the code browser's access rules, a timeout, a size cap, and a concurrency limit.
- REST API for the user, keys, tokens, organizations, members, repositories, settings, collaborators, refs, history, and file contents.
- API cursor pagination with `Link` headers, rate limits with `RateLimit-*` headers, and `Idempotency-Key` on mutating requests.
- OpenAPI document at `/api/v1/openapi.json`, committed as `docs/api/openapi.json`; contract tests check real responses against it.
- Append-only audit log of sign-ins, passwords, sudo, tokens, keys, organizations, and repositories, with no secrets; old entries are purged.
- Site administration at `/-/admin`: create, suspend, and promote users, restore deleted repositories, and view the audit log and health.
- Suspending a user signs them out everywhere and revokes their tokens.
- Organization owners see their organization's audit log at `/<org>/-/audit`.
- `slath backup create` takes a consistent backup while the server runs, without `pg_dump`, with a checksummed manifest.
- `slath backup restore` verifies the whole backup first, never takes repository config from it, and reconciles afterwards.
- `docs/admin/backup.md` and `just test-backup-restore`, the restore drill.
- Release container image with the embedded UI, a pinned Git, and OpenSSH, running as an unprivileged user.
- `deploy/compose/` (PostgreSQL, Slath, sshd, optional Caddy TLS), `deploy/systemd/`, `deploy/slath.example.toml`, and `docs/admin/install.md`.
- `slath admin gen-key --output FILE` writes a new master key with mode 0600 and never replaces an existing one.
- `just test-compose` follows the install guide end to end and renders every page of the image with outbound networking disabled.
- Repository maintenance runs `git gc` after 50 pushes or daily, never twice at once on a repository, and records the repository size.
- Pushes warn when a repository exceeds `limits.repo_soft_quota_bytes`.
- Every HTTP route declares its access level; Clippy rejects routes without one.
- Security tests sweep every route as anonymous and as a stranger, and check the authorization matrix on every surface, including SSH.
- A log test checks that passwords, tokens, cookies, and keys never reach the log, even at debug level.
- Regression tests modeled on recent advisories in other forges and in Git, covering injection, traversal, authorization, XSS, and CSRF.
- Appearance menu in the header for theme and density, saved in cookies or the account, and rendered by the server for the first paint.
- Settings → Appearance sets theme, density, and code font size; touch screens get 44 px targets in every density.
- Weave components: menus, dialogs, drawers, toasts, and loading and error states, shown on the component catalog.
- `just css-lint` rejects raw colors, font sizes, spacing, and shadows outside the design tokens.

### Fixed

- API tokens without `user:read` could list the user's organizations and their members.
- API tokens without `repo:read` could list the user's private repositories.

[Unreleased]: https://github.com/slath-org/slath/compare/v0.1.1...HEAD
[0.1.1]: https://github.com/slath-org/slath/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/slath-org/slath/releases/tag/v0.1.0
