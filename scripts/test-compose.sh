#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
# SPDX-License-Identifier: AGPL-3.0-or-later
#
# Install conformance for the container profile, following
# docs/admin/install.md: build the image, start deploy/compose, create the
# first admin with the CLI, check that the SSH login can reach nothing but
# the IPC socket, then (tests/ops/compose.rs) sign in, create a repository
# through the API, and clone it over HTTP and SSH. Afterwards the
# release image renders every page with outbound networking disabled
# (scripts/test-offline.sh).
#
# Usage: scripts/test-compose.sh   (needs docker or podman with compose)

set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"
ENGINE="${SLATH_CONTAINER_ENGINE:-$(command -v docker >/dev/null && echo docker || echo podman)}"
PROJECT="slath-conf-$$"
HTTP_PORT="${SLATH_TEST_HTTP_PORT:-38080}"
SSH_PORT="${SLATH_TEST_SSH_PORT:-38022}"
ADMIN_PASSWORD="compose admin password $$"

export SLATH_IMAGE="${SLATH_IMAGE:-slath:test}"
export SLATH_DB_PASSWORD="test-$(od -An -N12 -tx1 /dev/urandom | tr -d ' \n')"
export SLATH_PUBLIC_URL="http://127.0.0.1:$HTTP_PORT"
export SLATH_HTTP_BIND="127.0.0.1:$HTTP_PORT"
export SLATH_SSH_BIND="127.0.0.1:$SSH_PORT"
export SLATH_SSH_PORT="$SSH_PORT"

compose() { "$ENGINE" compose -p "$PROJECT" -f deploy/compose/compose.yml "$@"; }
cleanup() {
    if [[ "${KEEP:-0}" != 1 ]]; then
        compose logs --no-color slath sshd >"$ROOT/target/compose.log" 2>&1 || true
        compose down -v --remove-orphans >/dev/null 2>&1 || true
    fi
}
trap cleanup EXIT
mkdir -p target

compose build
compose up -d
for _ in $(seq 1 120); do
    curl -fsS "$SLATH_PUBLIC_URL/-/readyz" >/dev/null 2>&1 && break
    sleep 1
done
curl -fsS "$SLATH_PUBLIC_URL/-/readyz" >/dev/null || { compose logs slath; exit 1; }

printf '%s' "$ADMIN_PASSWORD" | compose exec -T slath \
    slath admin create-user --admin --username ada --email ada@example.org --password-stdin

# The SSH login `git` is a user of its own: it may use the IPC socket, and
# can't read the server's data, secrets, or environment. sshd has no data.
compose exec -T slath slath config check
denied() {
    if compose exec -T "$@" >/dev/null 2>&1; then
        echo "compose: '$*' must fail" >&2
        exit 1
    fi
}
[[ "$(compose exec -T slath id -u git)" != "$(compose exec -T slath id -u slath)" ]]
[[ " $(compose exec -T slath id -nG git) " != *" slath "* ]]
denied -u git slath cat /var/lib/slath/master.key
denied -u git slath ls /var/lib/slath
denied -u git slath ls /run/slath
denied -u git slath cat /proc/1/environ
denied sshd test -e /var/lib/slath/master.key
compose exec -T -u git sshd slath ssh-keys --socket /run/slath/slath.sock \
    SHA256:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA
denied -u nobody sshd slath ssh-keys --socket /run/slath/slath.sock \
    SHA256:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA

SLATH_COMPOSE_URL="$SLATH_PUBLIC_URL" SLATH_COMPOSE_SSH_PORT="$SSH_PORT" \
    SLATH_COMPOSE_ADMIN_PASSWORD="$ADMIN_PASSWORD" \
    cargo nextest run --locked --profile ci -p slath-tests --test ops --run-ignored only -E 'test(conf_ops_01)'
# Kept for `just conformance-report` (the next ci-profile run overwrites the original).
mkdir -p target/compose && cp target/nextest/ci/junit.xml target/compose/junit.xml

scripts/test-offline.sh "$SLATH_IMAGE"
echo "compose: ok"
