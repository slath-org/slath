<!-- SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de> -->
<!-- SPDX-License-Identifier: CC-BY-SA-4.0 -->
# Developer setup

This gets a clean Linux machine from zero to a green `just check`.

## Prerequisites

- Rust via [rustup](https://rustup.rs). `rust-toolchain.toml` pins the version,
  and rustup installs it on first use.
- The newest stable Git release (currently 2.56.0); before 1.0 Slath
  requires it. `scripts/install-git.sh 2.56.0` builds it if your distribution
  lags behind, and `SLATH_TEST_GIT` points the tests at it.
- Python ≥ 3.11 (helper scripts and their tests).
- Node.js ≥ 22 (`web/.nvmrc` names the version CI uses) to build the UI
  bundle that the server embeds. It's needed at build time only.
- Docker (with the compose plugin) or Podman (with `podman compose`) for the
  development database.

## Steps

```sh
cargo install --locked just     # once, if you don't have just yet
just setup                      # installs cargo-nextest, cargo-deny, sqlx-cli; creates .env
just dev-db                     # starts PostgreSQL 18 on 127.0.0.1:54329
just tools                      # installs the pinned jj versions for the compat tests
just check                      # UI bundle, fmt, clippy, tests, cargo deny, guards
just test-e2e                   # browser tests (Playwright container, all browsers)
```

`cargo install` puts binaries in `~/.cargo/bin`, which must be on `PATH`.

The compatibility tests also need Docker (or Podman, via
`SLATH_CONTAINER_ENGINE=podman`) for the SSH server fixture. Set
`SLATH_SKIP_SSH_TESTS=1` or `SLATH_SKIP_JJ_TESTS=1` to skip those suites.

## Configuration

`.env` (copied from `.env.example`, ignored by Git) sets `DATABASE_URL` for
tests and `sqlx`. To use another port, set `SLATH_DEV_DB_PORT` before
`just dev-db` and adjust `DATABASE_URL` to match.

To use Podman instead of Docker when both are installed, set
`SLATH_CONTAINER_ENGINE=podman`.

## Useful recipes

| Recipe | Does |
|---|---|
| `just test -E 'test(name)'` | Run matching tests (any nextest filter works) |
| `just test -p slath-git` | Run one crate's tests |
| `just fmt` | Format all code |
| `just web` | Rebuild the UI bundle after changing `web/src` |
| `just test-e2e -g name` | Run matching browser tests; `E2E_LOCAL=1` runs Playwright without its container |
| `just css-lint` | Check CSS against the design-token rules |
| `just test-backup-restore` | Run the backup and restore drill |
| `docker build -t slath:test . && just test-compose` | Install with Docker Compose as the guide says, then render every page offline |
| `just schema-fixture` | Write the release schema and data fixture |
| `just dev-db-stop` | Stop PostgreSQL, keeping its data |
| `just dev-db-reset` | Delete the development database volume |
