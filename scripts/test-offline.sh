#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
# SPDX-License-Identifier: AGPL-3.0-or-later
#
# Renders every page of the release image with outbound networking disabled.
# PostgreSQL, Slath, and a Playwright browser share one network namespace
# without any interface but loopback (`--network none`), so a page that needs
# anything from outside fails to load (tests/e2e/offline).
#
# Usage: scripts/test-offline.sh [IMAGE]   (default slath:test)

set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"
ENGINE="${SLATH_CONTAINER_ENGINE:-$(command -v docker >/dev/null && echo docker || echo podman)}"
IMAGE="${1:-slath:test}"
PW_VERSION="$(sed -nE 's/.*"@playwright\/test": "([^"]+)".*/\1/p' tests/e2e/package.json)"
NAME="slath-offline-$$"
PASSWORD="offline admin password $$"

cleanup() { "$ENGINE" rm -f "$NAME-app" "$NAME-db" >/dev/null 2>&1 || true; }
trap cleanup EXIT
[[ -d tests/e2e/node_modules ]] || (cd tests/e2e && npm ci)

"$ENGINE" run -d --name "$NAME-db" --network none \
    -e POSTGRES_USER=slath -e POSTGRES_PASSWORD=slath -e POSTGRES_DB=slath \
    docker.io/library/postgres:18 >/dev/null
for _ in $(seq 1 60); do
    "$ENGINE" exec "$NAME-db" pg_isready -q -h 127.0.0.1 -U slath && break
    sleep 1
done

# The admin pages are rendered too; they need a second factor by default,
# which this rendering check doesn't set up (sign-in with MFA is tested in
# the browser tests).
"$ENGINE" run -d --name "$NAME-app" --network "container:$NAME-db" \
    -e SLATH__DATABASE__URL=postgres://slath:slath@127.0.0.1/slath \
    -e SLATH__AUTH__REQUIRE_MFA_FOR_ADMINS=false \
    -e SLATH__SERVER__LISTEN=127.0.0.1:3000 \
    -e SLATH__SERVER__PUBLIC_URL=http://127.0.0.1:3000 \
    --entrypoint /bin/sh "$IMAGE" \
    -c 'slath admin gen-key --output /var/lib/slath/master.key && exec slath serve --migrate' >/dev/null
for _ in $(seq 1 60); do
    "$ENGINE" logs "$NAME-app" 2>&1 | grep -q 'listening on' && break
    sleep 1
done
printf '%s' "$PASSWORD" | "$ENGINE" exec -i "$NAME-app" \
    slath admin create-user --admin --username ada --email ada@example.org --password-stdin >/dev/null

playwright() {
    "$ENGINE" run --rm --network "container:$NAME-db" --ipc host --init \
        --user "$(id -u):$(id -g)" -e HOME=/tmp \
        -e SLATH_OFFLINE_URL=http://127.0.0.1:3000 -e SLATH_OFFLINE_PASSWORD="$PASSWORD" \
        -v "$ROOT:/work" -w /work/tests/e2e \
        "mcr.microsoft.com/playwright:v${PW_VERSION}-noble" \
        npx playwright test -c offline/playwright.config.ts "$@"
}
playwright -g setup

# Content for the code pages: pushed straight into the repository, then
# recorded by the reconciler.
id="$("$ENGINE" exec "$NAME-db" psql -U slath -tAc "SELECT id FROM repositories WHERE name = 'offline'")"
repo="/var/lib/slath/repos/${id:0:2}/$id.git"
"$ENGINE" exec "$NAME-app" /bin/sh -c "
    set -e
    g='/opt/git/bin/git -c user.name=Test -c user.email=test@example.org -c init.defaultBranch=main'
    rm -rf /tmp/w && \$g init -q /tmp/w && cd /tmp/w
    mkdir src
    printf '# Offline\n\nSome *Markdown* with \`code\` and a [link](https://example.org).\n\n\`\`\`rust\nfn main() {}\n\`\`\`\n' > README.md
    printf 'fn main() {\n    println!(\"hello\");\n}\n' > src/main.rs
    \$g add . && \$g commit -q -m first
    printf 'fn main() {\n    println!(\"hello, world\");\n}\n' > src/main.rs
    \$g commit -q -am second
    \$g push -q $repo HEAD:refs/heads/main
    slath fsck --reconcile >/dev/null"
playwright -g pages
echo "offline: ok"
