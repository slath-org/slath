# SPDX-FileCopyrightText: 2026 Erich Seifert
# SPDX-License-Identifier: AGPL-3.0-or-later
# Task runner for Slath. `just --list` shows all recipes.

set shell := ["bash", "-euo", "pipefail", "-c"]
set dotenv-load

# Container engine for dev services: docker or podman.
engine := env("SLATH_CONTAINER_ENGINE", if `command -v docker || true` != "" { "docker" } else { "podman" })

# Run every check a pull request must pass.
default: check

# fmt, clippy, tests, and the planning-doc check.
check: fmt-check clippy test deny license-deps guards

fmt-check:
    cargo fmt --all --check

fmt:
    cargo fmt --all

clippy:
    cargo clippy --workspace --all-targets --locked -- -D warnings

# Run tests; pass extra nextest args, e.g. `just test -E 'test(name)'`.
test *ARGS:
    cargo nextest run --workspace --locked --no-tests=pass {{ARGS}}

# Dependency policy: licenses, advisories, bans, sources.
deny:
    cargo deny check

# Apache-2.0 crates must not depend on AGPL crates.
license-deps:
    python3 scripts/check_license_deps.py
    python3 -m unittest -q scripts/test_check_license_deps.py

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

# Start the development PostgreSQL and wait until it accepts connections.
dev-db:
    {{engine}} compose -f deploy/dev/compose.yml up -d --wait postgres

# Stop the development services (data is kept in a volume).
dev-db-stop:
    {{engine}} compose -f deploy/dev/compose.yml down

# Delete the development database volume.
dev-db-reset:
    {{engine}} compose -f deploy/dev/compose.yml down -v
