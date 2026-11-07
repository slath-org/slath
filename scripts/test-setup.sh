#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
# SPDX-License-Identifier: AGPL-3.0-or-later
#
# Install conformance for the systemd profile's `slath setup`: builds a clean
# Debian host image (tests/ops/setup-host) with the slath binary from the
# release image (`docker build -t slath:test .` first, or set SLATH_IMAGE),
# then runs tests/ops/setup.rs against it.
#
# Usage: scripts/test-setup.sh   (needs docker or podman)

set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"
ENGINE="${SLATH_CONTAINER_ENGINE:-$(command -v docker >/dev/null && echo docker || echo podman)}"
SLATH_IMAGE="${SLATH_IMAGE:-slath:test}"
HOST_IMAGE="slath-setup-host:test"

"$ENGINE" image inspect "$SLATH_IMAGE" >/dev/null 2>&1 || {
    echo "setup: image $SLATH_IMAGE not found; build it first: docker build -t $SLATH_IMAGE ." >&2
    exit 1
}
"$ENGINE" build -q --build-arg "SLATH_IMAGE=$SLATH_IMAGE" -t "$HOST_IMAGE" tests/ops/setup-host >/dev/null
cargo build --locked -p slath-tests --bins
SLATH_CONTAINER_ENGINE="$ENGINE" SLATH_SETUP_HOST_IMAGE="$HOST_IMAGE" \
    cargo nextest run --locked --profile ci -p slath-tests --test ops --run-ignored only -E 'test(conf_ops_10_setup)'
mkdir -p target/setup && cp target/nextest/ci/junit.xml target/setup/junit.xml
echo "setup: ok"
