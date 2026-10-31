# SPDX-FileCopyrightText: 2026 Erich Seifert
# SPDX-License-Identifier: AGPL-3.0-or-later
# Task runner for Slath. `just --list` shows all recipes.

set shell := ["bash", "-euo", "pipefail", "-c"]

# Run every check a pull request must pass.
default: check

# fmt, clippy, tests, and the planning-doc check.
check: fmt-check clippy test

fmt-check:
    cargo fmt --all --check

fmt:
    cargo fmt --all

clippy:
    cargo clippy --workspace --all-targets --locked -- -D warnings

# Run tests; pass extra nextest args, e.g. `just test -E 'test(name)'`.
test *ARGS:
    cargo nextest run --workspace --locked --no-tests=pass {{ARGS}}
