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
check: tokens-check web bundle-size fmt-check clippy api-check test sqlx-check deny license-deps py-test asset-licenses upgrade-notes guards css-lint

# Regenerate web/src/tokens.css from web/src/tokens.json.
tokens:
    python3 scripts/build_tokens.py
    python3 scripts/check_contrast.py

# Fail if tokens.css is stale or any declared color pair lacks contrast.
tokens-check:
    python3 scripts/build_tokens.py --check
    python3 scripts/check_contrast.py

# Build the UI bundle (web/ -> web/dist), which the server embeds. Needs Node.js >= 22.
web:
    @node -e 'if (+process.versions.node.split(".")[0] < 22) { console.error("Node.js >= 22 is required (see web/.nvmrc), found " + process.version); process.exit(1) }'
    cd web && { [ -d node_modules ] || npm ci; } && npm run build

# The Markdown editor stays within its download budget per mode.
bundle-size:
    python3 scripts/check_bundle_size.py

# Browser tests (Playwright, all browsers and matrix cells). Playwright runs in
# its container unless E2E_LOCAL=1. Pass Playwright args, e.g. `-g name`.
test-e2e *ARGS: web
    cargo build --locked -p slath -p slath-tests --bins
    scripts/e2e.sh {{ARGS}}

# Restore drill: back up a populated instance during pushes, restore it to an
# empty database and data directory, and compare; plus hostile backups.
test-backup-restore:
    cargo build --locked -p slath --bins
    cargo nextest run --locked -p slath-tests --test ops -E 'test(/backup|restore/)'

# Upgrade every release (built from its tag, cached in target/upgrade-matrix)
# to this checkout and check the data. Pass tags to limit it.
upgrade-matrix *TAGS:
    scripts/upgrade-matrix.sh {{TAGS}}

# Write tests/fixtures/schema/v<version>.sql (schema and data of a populated
# instance) when preparing a release. Needs the dev database container.
schema-fixture:
    cargo nextest run --locked -p slath-tests --test ops --run-ignored only -E 'test(write_schema_fixture)'

# Install conformance with deploy/compose plus the offline render check of the
# release image (`docker build -t slath:test .` first, or set SLATH_IMAGE).
test-compose:
    scripts/test-compose.sh

# Weave restraint rules: no raw colors, font sizes, or spacing in CSS.
css-lint:
    cd web && {{ "{" }} [ -d node_modules ] || npm ci; {{ "}" }} && npm run --silent lint:css

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

# The committed OpenAPI document matches the code. Set
# SLATH_UPDATE_SNAPSHOTS=1 to regenerate docs/api/openapi.json.
api-check:
    cargo nextest run --locked -p slath-http --test openapi

# Every bundled font, icon set, and UI dependency has a license entry.
asset-licenses:
    python3 scripts/check_asset_licenses.py

# Every release tag has upgrade notes, and every notes file is well-formed.
upgrade-notes:
    python3 scripts/check_upgrade_notes.py

# Unit tests of the Python helper scripts.
py-test:
    python3 -m unittest -q scripts/test_*.py

# Run all tests with JUnit output and map them to the cases in CASES (a
# Markdown file with case tables). Pass `--gate M<n>` to fail unless that gate is met.
# Browser and compose results are included when `just test-e2e` and
# `just test-compose` have written them.
conformance-report CASES *ARGS:
    cargo nextest run --workspace --locked --profile ci --no-tests=pass || true
    python3 scripts/conformance_report.py --cases {{CASES}} --junit target/nextest/ci/junit.xml \
        $(for f in target/e2e/junit.xml target/compose/junit.xml; do [ -f "$f" ] && printf -- '--junit %s ' "$f"; done) {{ARGS}}

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

# Start the development mail catcher: SMTP on 127.0.0.1:1025, inbox at
# http://127.0.0.1:8025.
dev-mail:
    {{engine}} compose -f deploy/dev/compose.yml up -d mail

# Stop the development services (data is kept in a volume).
dev-db-stop:
    {{engine}} compose -f deploy/dev/compose.yml down

# Delete the development database volume.
dev-db-reset:
    {{engine}} compose -f deploy/dev/compose.yml down -v
