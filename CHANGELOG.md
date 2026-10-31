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
  Git ≥ 2.43 check), validators for user revisions, ref names, and paths, an
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
