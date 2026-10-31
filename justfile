# SPDX-FileCopyrightText: 2026 Erich Seifert
# SPDX-License-Identifier: AGPL-3.0-or-later
# Task runner for Slath. `just --list` shows all recipes.

set shell := ["bash", "-euo", "pipefail", "-c"]

# Run every check a pull request must pass.
default: check

# fmt, clippy, tests, and the planning-doc check.
check: fmt-check clippy test deny license-deps

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
