#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
# SPDX-License-Identifier: AGPL-3.0-or-later
#
# Conformance for the agent sandbox (OPS-08), following its compose.yml
# with an image built beforehand (`docker build -t slath:test .`, or
# SLATH_IMAGE): one `up` yields a bot token in secrets/bot-token, mode
# 0600, that clones the seeded repository, pushes a branch, and opens a
# merge request through the API; starting again changes nothing.
#
# Usage: scripts/test-sandbox.sh   (needs docker or podman with compose, git, curl)

set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ENGINE="${SLATH_CONTAINER_ENGINE:-$(command -v docker >/dev/null && echo docker || echo podman)}"
PROJECT="slath-sandbox-$$"
export SLATH_IMAGE="${SLATH_IMAGE:-slath:test}"
export SANDBOX_HTTP_PORT="${SLATH_TEST_HTTP_PORT:-38180}"
export SANDBOX_SSH_PORT="${SLATH_TEST_SSH_PORT:-38122}"
URL="http://127.0.0.1:$SANDBOX_HTTP_PORT"

"$ENGINE" image inspect "$SLATH_IMAGE" >/dev/null 2>&1 || {
    echo "sandbox: image $SLATH_IMAGE not found; build it first: docker build -t $SLATH_IMAGE ." >&2
    exit 1
}

# A copy, as someone would download it, so the test writes no secrets
# into the source tree.
WORK="$ROOT/target/sandbox-$$"
mkdir -p "$WORK"
cp "$ROOT/deploy/compose/postgres-init.sh" "$WORK/"
cp -r "$ROOT/deploy/compose/agent-sandbox" "$WORK/"
DIR="$WORK/agent-sandbox"
rm -f "$DIR"/secrets/*

compose() { (cd "$DIR" && "$ENGINE" compose -p "$PROJECT" "$@"); }
cleanup() {
    if [[ "${KEEP:-0}" != 1 ]]; then
        compose logs --no-color >"$ROOT/target/sandbox.log" 2>&1 || true
        compose down -v --remove-orphans >/dev/null 2>&1 || true
        rm -rf "$WORK"
    fi
}
trap cleanup EXIT

seed() {
    compose up -d
    local status
    # Compose prints the exit code, or (v5) "… exited with status code N".
    status=$(compose wait seed | tail -n 1)
    [[ "$status" =~ (^|status code )0$ ]] || { compose logs seed >&2; echo "sandbox: seed: $status" >&2; exit 1; }
}

seed
compose logs --no-color --no-log-prefix seed | grep -q "Slath agent sandbox" || { echo "sandbox: no greeting" >&2; exit 1; }
for f in bot-token ada-password; do
    mode=$(stat -c %a "$DIR/secrets/$f")
    [[ "$mode" == 600 ]] || { echo "sandbox: secrets/$f has mode $mode" >&2; exit 1; }
done
TOKEN=$(cat "$DIR/secrets/bot-token")
[[ "$TOKEN" == slath_bot_* ]] || { echo "sandbox: no bot token" >&2; exit 1; }

# The bot clones, pushes a branch, and opens a merge request.
CLONE="$WORK/playground"
git -c init.defaultBranch=main clone -q "http://agent:$TOKEN@127.0.0.1:$SANDBOX_HTTP_PORT/sandbox/playground.git" "$CLONE"
git -C "$CLONE" -c user.name=agent -c user.email=agent@sandbox.invalid commit -q --allow-empty -m "Start"
git -C "$CLONE" push -q origin HEAD:main
echo "by an agent" >"$CLONE/notes.txt"
git -C "$CLONE" add notes.txt
git -C "$CLONE" -c user.name=agent -c user.email=agent@sandbox.invalid commit -q -m "Add notes"
git -C "$CLONE" push -q origin HEAD:agent/notes
status=$(curl -s -o "$WORK/mr.json" -w '%{http_code}' -X POST "$URL/api/v1/repos/sandbox/playground/merge-requests" \
    -H "Authorization: Bearer $TOKEN" -H "Content-Type: application/json" \
    -d '{"source_branch": "agent/notes", "title": "Add notes"}')
[[ "$status" == 201 ]] || { cat "$WORK/mr.json" >&2; echo "sandbox: opening the merge request gave $status" >&2; exit 1; }

# Starting again changes nothing and keeps the token.
seed
compose logs --no-color --no-log-prefix seed | grep -qx "0 changes" || { compose logs seed >&2; echo "sandbox: the second start changed something" >&2; exit 1; }
[[ "$(cat "$DIR/secrets/bot-token")" == "$TOKEN" ]] || { echo "sandbox: the token changed" >&2; exit 1; }
echo "sandbox: ok"
