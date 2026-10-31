# SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
# SPDX-License-Identifier: AGPL-3.0-or-later
# Task runner for Slath. `just --list` shows all recipes.

set shell := ["bash", "-euo", "pipefail", "-c"]
set dotenv-load

# Container engine for dev services: docker or podman.
engine := env("SLATH_CONTAINER_ENGINE", if `command -v docker || true` != "" { "docker" } else { "podman" })

# Run every check a pull request must pass.
default: check

# fmt, clippy, tests, and the planning-doc check.
check: fmt-check clippy test sqlx-check deny license-deps py-test guards

fmt-check:
    cargo fmt --all --check

fmt:
    cargo fmt --all

clippy:
    cargo clippy --workspace --all-targets --locked -- -D warnings

# Run tests; pass extra nextest args, e.g. `just test -E 'test(name)'`.
test *ARGS:
    cargo nextest run --workspace --locked --no-tests=pass {{ARGS}}

# Regenerate .sqlx/ offline query data after changing a query (needs `just dev-db`).
sqlx-prepare:
    cargo sqlx prepare --workspace -- --all-targets

# Fail if .sqlx/ is stale.
sqlx-check:
    cargo sqlx prepare --workspace --check -- --all-targets

# Apply migrations to the development database.
migrate:
    cargo sqlx migrate run --source migrations

# Dependency policy: licenses, advisories, bans, sources.
deny:
    cargo deny check

# Apache-2.0 crates must not depend on AGPL crates.
license-deps:
    python3 scripts/check_license_deps.py

# Unit tests of the Python helper scripts.
py-test:
    python3 -m unittest -q scripts/test_*.py

# Run all tests with JUnit output and map them to the cases in CASES (a
# Markdown file with case tables). Pass `--gate M<n>` to fail unless that gate is met.
conformance-report CASES *ARGS:
    cargo nextest run --workspace --locked --profile ci --no-tests=pass || true
    python3 scripts/conformance_report.py --cases {{CASES}} --junit target/nextest/ci/junit.xml {{ARGS}}

# Static guards (SPDX, Git spawns, shells, |safe, external URLs) and their tests.
guards:
    scripts/guards.sh
    scripts/guards_test.sh

# Install the cargo tools the recipes use (needs rustup or cargo).
setup:
    cargo install --locked just cargo-nextest cargo-deny
    cargo install --locked sqlx-cli --no-default-features --features postgres,rustls
    [ -f .env ] || cp .env.example .env
    @echo "Tools installed. Make sure ~/.cargo/bin is on PATH."

# Install the pinned jj versions the compatibility tests run against.
tools:
    for v in $(python3 -c 'import tomllib; print(" ".join(e["version"] for e in tomllib.load(open("tests/compat/versions.toml", "rb"))["jj"]))'); do scripts/install-jj.sh "$v"; done

# Start the development PostgreSQL and wait until it accepts connections.
dev-db:
    {{engine}} compose -f deploy/dev/compose.yml up -d --wait postgres

# Stop the development services (data is kept in a volume).
dev-db-stop:
    {{engine}} compose -f deploy/dev/compose.yml down

# Delete the development database volume.
dev-db-reset:
    {{engine}} compose -f deploy/dev/compose.yml down -v
