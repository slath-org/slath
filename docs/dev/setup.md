<!-- SPDX-FileCopyrightText: 2026 Erich Seifert -->
<!-- SPDX-License-Identifier: CC-BY-SA-4.0 -->
# Developer setup

This gets a clean Linux machine from zero to a green `just check`.

## Prerequisites

- Rust via [rustup](https://rustup.rs). `rust-toolchain.toml` pins the version,
  and rustup installs it on first use.
- Git ≥ 2.43.
- Python ≥ 3.11 (helper scripts and their tests).
- Docker (with the compose plugin) or Podman (with `podman compose`) for the
  development database.

## Steps

```sh
cargo install --locked just     # once, if you don't have just yet
just setup                      # installs cargo-nextest, cargo-deny, sqlx-cli; creates .env
just dev-db                     # starts PostgreSQL 18 on 127.0.0.1:54329
just check                      # fmt, clippy, tests, cargo deny, guards
```

`cargo install` puts binaries in `~/.cargo/bin`, which must be on `PATH`.

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
| `just dev-db-stop` | Stop PostgreSQL, keeping its data |
| `just dev-db-reset` | Delete the development database volume |
