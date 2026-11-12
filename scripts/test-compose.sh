#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
# SPDX-License-Identifier: AGPL-3.0-or-later
#
# Install conformance for the container profile, following
# docs/admin/install.md with an image built beforehand (`docker build -t
# slath:test .`, or SLATH_IMAGE): an .env with only the database password
# and the public URL, an update on start from a database at the upgrade
# baseline (preflight, backup into the backups volume, migrations), the
# first admin with one `docker compose run`, a check that the SSH login can
# reach nothing but the IPC socket, then (tests/ops/compose.rs) sign in,
# create a repository through the API, and clone it over HTTP and SSH.
# Afterwards the image renders every page with outbound networking
# disabled (scripts/test-offline.sh).
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
# Only the ports differ from a real installation (several runs may share a host).
export SLATH_HTTP_BIND="127.0.0.1:$HTTP_PORT"
export SLATH_SSH_BIND="127.0.0.1:$SSH_PORT"
export SLATH_SSH_PORT="$SSH_PORT"
SLATH_PUBLIC_URL="http://127.0.0.1:$HTTP_PORT"
mkdir -p target
ENV_FILE="$ROOT/target/compose-$$.env"
# The whole .env of an installation: two values.
printf 'SLATH_DB_PASSWORD=%s\nSLATH_PUBLIC_URL=%s\n' \
    "test-$(od -An -N12 -tx1 /dev/urandom | tr -d ' \n')" "$SLATH_PUBLIC_URL" >"$ENV_FILE"

"$ENGINE" image inspect "$SLATH_IMAGE" >/dev/null 2>&1 || {
    echo "compose: image $SLATH_IMAGE not found; build it first: docker build -t $SLATH_IMAGE ." >&2
    exit 1
}

compose() { "$ENGINE" compose -p "$PROJECT" --env-file "$ENV_FILE" -f deploy/compose/compose.yml "$@"; }
cleanup() {
    if [[ "${KEEP:-0}" != 1 ]]; then
        compose logs --no-color slath sshd >"$ROOT/target/compose.log" 2>&1 || true
        compose down -v --remove-orphans >/dev/null 2>&1 || true
        rm -f "$ENV_FILE"
    fi
}
trap cleanup EXIT

# An existing installation at the upgrade baseline: its database and its
# repositories (empty, as the fixture has no Git data).
compose up -d --wait postgres
compose exec -T postgres psql -q -v ON_ERROR_STOP=1 -U slath -d slath \
    <tests/fixtures/schema/baseline.sql >/dev/null
repos=$(compose exec -T postgres psql -At -U slath -d slath \
    -c "SELECT CASE WHEN state = 'deleting' THEN 'trash/' || id || '.git'
                    ELSE 'repos/' || left(id::text, 2) || '/' || id || '.git' END FROM repositories")
compose run --rm --no-deps -T --entrypoint /bin/sh slath -c \
    'for r in "$@"; do /opt/git/bin/git init -q --bare "/var/lib/slath/$r"; done' sh $repos

# Starting this version updates it: preflight, backup, migrations.
compose up -d
for _ in $(seq 1 120); do
    curl -fsS "$SLATH_PUBLIC_URL/-/readyz" >/dev/null 2>&1 && break
    sleep 1
done
curl -fsS "$SLATH_PUBLIC_URL/-/readyz" >/dev/null || { compose logs slath; exit 1; }
compose logs --no-color slath | grep -q "update applied" || { compose logs slath; exit 1; }
backups=$(compose exec -T slath ls /var/lib/slath/backups)
[[ "$backups" == pre-upgrade-* ]] || { echo "compose: no pre-upgrade backup: $backups" >&2; exit 1; }

# Both services report health to the engine (Docker or Podman).
healthy() {
    local id status=""
    id=$(compose ps -q "$1")
    for _ in $(seq 1 90); do
        status=$("$ENGINE" inspect --format '{{.State.Health.Status}}' "$id" 2>/dev/null || true)
        [[ "$status" == healthy ]] && return 0
        sleep 1
    done
    echo "compose: $1 is ${status:-without health} instead of healthy" >&2
    exit 1
}
healthy slath
healthy sshd

# The first administrator: one `docker compose run`.
printf '%s' "$ADMIN_PASSWORD" | compose run --rm -T slath \
    admin create-user --admin --username ada --email ada@example.org --password-stdin

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

# Slath's database role only owns its database, the superuser can't log in
# over the network, and `slath doctor` agrees. (Whether the image has the
# newest PostgreSQL release depends on upstream timing, so it isn't checked.)
[[ "$(compose exec -T postgres psql -At -U slath -d slath -c 'SELECT rolsuper OR rolcreaterole OR rolcreatedb FROM pg_roles WHERE rolname = current_user')" == f ]] ||
    { echo "compose: slath's database role is privileged" >&2; exit 1; }
pw=$(sed -n 's/^SLATH_DB_PASSWORD=//p' "$ENV_FILE")
denied -e PGPASSWORD="$pw" postgres psql -h 127.0.0.1 -U postgres -d postgres -c 'SELECT 1'
# (Doctor exits 1 for any failure; only this code matters here.)
doctor=$(compose exec -T slath slath doctor --output json || true)
python3 -c '
import json, sys
bad = [c for c in json.loads(sys.argv[1])["checks"]
       if c["code"] == "postgres.superuser" and c["status"] != "ok"]
sys.exit(f"compose: doctor reports {bad}" if bad else 0)' "$doctor"

SLATH_COMPOSE_URL="$SLATH_PUBLIC_URL" SLATH_COMPOSE_SSH_PORT="$SSH_PORT" \
    SLATH_COMPOSE_ADMIN_PASSWORD="$ADMIN_PASSWORD" \
    cargo nextest run --locked --profile ci -p slath-tests --test ops --run-ignored only -E 'test(conf_ops_01)'
# Kept for `just conformance-report` (the next ci-profile run overwrites the original).
mkdir -p target/compose && cp target/nextest/ci/junit.xml target/compose/junit.xml

scripts/test-offline.sh "$SLATH_IMAGE"
echo "compose: ok"
