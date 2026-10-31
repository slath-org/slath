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
  minimum Git version check), validators for user revisions, ref names, and paths, an
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
  `just conformance-report [--gate M<n>]`, which maps `conf_<area>_<nn>_*`
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

### Changed

- Before 1.0, Slath requires the newest stable Git release (currently 2.56.0)
  instead of 2.43, and the compatibility matrix pins only that release.
