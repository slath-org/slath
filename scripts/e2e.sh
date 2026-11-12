#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
# SPDX-License-Identifier: AGPL-3.0-or-later
#
# Runs the browser tests (tests/e2e). The server runs on this host (fresh
# database and data dir, via slath-e2e-server); Playwright runs either in the
# official Playwright container (default, all three browsers on any host) or
# directly when E2E_LOCAL=1 (needs `npx playwright install --with-deps`).
#
# Usage: scripts/e2e.sh [playwright args...]

set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"
PW_VERSION="$(sed -nE 's/.*"@playwright\/test": "([^"]+)".*/\1/p' tests/e2e/package.json)"
ENGINE="${SLATH_CONTAINER_ENGINE:-$(command -v docker >/dev/null && echo docker || echo podman)}"

[[ -d tests/e2e/node_modules ]] || (cd tests/e2e && npm ci)

# Run Playwright as the calling user, so results in the checkout stay
# theirs: rootless Podman maps the user itself (`--userns=keep-id`); with
# Docker or rootful Podman the IDs are passed.
if [[ "$ENGINE" == *podman* && "$("$ENGINE" info --format '{{.Host.Security.Rootless}}' 2>/dev/null)" == true ]]; then
    as_user=(--userns=keep-id)
else
    as_user=(--user "$(id -u):$(id -g)")
fi

server_out="$(mktemp)"
coproc SERVER { exec target/debug/slath-e2e-server >"$server_out"; }
cleanup() {
    exec {SERVER[1]}>&- 2>/dev/null || true
    wait "$SERVER_PID" 2>/dev/null || true
    rm -f "$server_out"
}
trap cleanup EXIT
for _ in $(seq 1 600); do
    url="$(sed -nE 's/^SLATH_E2E_URL=(.*)$/\1/p' "$server_out")"
    [[ -n "$url" ]] && break
    sleep 0.1
done
[[ -n "${url:-}" ]] || { echo "e2e: server did not start" >&2; exit 1; }
echo "e2e: server at $url"

if [[ "${E2E_LOCAL:-0}" == 1 ]]; then
    (cd tests/e2e && SLATH_E2E_URL="$url" npx playwright test "$@")
else
    "$ENGINE" run --rm --network host --ipc host --init \
        "${as_user[@]}" -e HOME=/tmp -e CI="${CI:-}" \
        -e SLATH_E2E_URL="$url" \
        -v "$ROOT:/work:z" -w /work/tests/e2e \
        "mcr.microsoft.com/playwright:v${PW_VERSION}-noble" \
        npx playwright test "$@"
fi
