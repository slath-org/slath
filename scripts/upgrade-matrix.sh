#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
# SPDX-License-Identifier: AGPL-3.0-or-later
#
# The upgrade matrix: builds the `slath` binary of every release tag since
# the upgrade baseline v0.3.0 (cached in target/upgrade-matrix/<tag>/), then
# runs tests/ops/upgrade_matrix.rs, which upgrades an instance populated by
# each release to this checkout. A new release tag extends the matrix by
# itself. The 0.1/0.2 previews aren't upgrade sources. Until a release since
# the baseline exists, the report marks the case N/A with that reason.
#
# Usage: scripts/upgrade-matrix.sh [TAG...]   (default: every vX.Y.Z tag ≥ v0.3.0)

set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"
OUT="$ROOT/target/upgrade-matrix"
mkdir -p "$OUT"

if [[ $# -gt 0 ]]; then
    TAGS=("$@")
else
    mapfile -t TAGS < <(git tag --list 'v*' | grep -E '^v[0-9]+\.[0-9]+\.[0-9]+$' | sort -V |
        awk -F'[v.]' '$2 > 0 || $3 >= 3')
fi
if [[ ${#TAGS[@]} -eq 0 ]]; then
    mkdir -p target/upgrade-matrix-report
    cat > target/upgrade-matrix-report/junit.xml <<'XML'
<?xml version="1.0" encoding="UTF-8"?>
<testsuites name="upgrade-matrix"><testsuite name="slath-tests::ops" tests="1" skipped="1">
<testcase name="upgrade_matrix::conf_ops_03_upgrade_from_every_release" classname="slath-tests::ops"><skipped message="N/A: no release since the upgrade baseline v0.3.0 to upgrade from"/></testcase>
</testsuite></testsuites>
XML
    echo "upgrade-matrix: N/A (no release since the upgrade baseline v0.3.0)"
    exit 0
fi

MATRIX=""
for tag in "${TAGS[@]}"; do
    bin="$OUT/$tag/slath"
    if [[ ! -x "$bin" ]]; then
        echo "== building $tag"
        tree="$OUT/src-$tag"
        rm -rf "$tree"
        git worktree add --detach -f "$tree" "$tag" >/dev/null
        (
            cd "$tree"
            (cd web && npm ci --no-audit --no-fund && npm run build)
            # Old releases build offline against their own query data.
            SQLX_OFFLINE=true CARGO_TARGET_DIR="$OUT/target" cargo build --release --locked -p slath
        )
        mkdir -p "$OUT/$tag"
        cp "$OUT/target/release/slath" "$bin"
        git worktree remove --force "$tree"
    fi
    MATRIX+="$tag=$bin "
done
git worktree prune

cargo build --locked -p slath -p slath-tests --bins
SLATH_UPGRADE_MATRIX="$MATRIX" cargo nextest run --locked --profile ci -p slath-tests --test ops \
    --run-ignored only -E 'test(conf_ops_03_upgrade_from_every_release)'
mkdir -p target/upgrade-matrix-report && cp target/nextest/ci/junit.xml target/upgrade-matrix-report/junit.xml
echo "upgrade-matrix: ok (${TAGS[*]})"
