# SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
# SPDX-License-Identifier: AGPL-3.0-or-later
#
# Shared setup for fixture generators. Source it, then call `fixture_init DEST`.
# Everything that feeds into object IDs is fixed: identities, dates, config,
# umask, and locale. Each `tick` advances the clock by one minute, so the same
# script always produces the same refs and OIDs.

set -euo pipefail

FIXTURES_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
GIT="${SLATH_FIXTURE_GIT:-git}"

export LC_ALL=C TZ=UTC
export GIT_CONFIG_NOSYSTEM=1 GIT_CONFIG_GLOBAL=/dev/null GIT_TERMINAL_PROMPT=0
export GIT_AUTHOR_NAME="Ada Author" GIT_AUTHOR_EMAIL="ada@example.org"
export GIT_COMMITTER_NAME="Cy Committer" GIT_COMMITTER_EMAIL="cy@example.org"
umask 022
_fixture_clock=1700000000

# Advances the fixed clock by one minute and exports it as both dates.
tick() {
    _fixture_clock=$((_fixture_clock + 60))
    export GIT_AUTHOR_DATE="@$_fixture_clock +0000" GIT_COMMITTER_DATE="@$_fixture_clock +0000"
}

g() { "$GIT" -c init.defaultBranch=main -c core.autocrlf=false -c core.fileMode=true \
    -c commit.gpgSign=false -c tag.gpgSign=false -c advice.detachedHead=false -c advice.nestedTag=false "$@"; }

# Creates an empty work repository at $1 and enters it.
fixture_init() {
    local dest="${1:?usage: $0 DEST}"
    if [[ -e "$dest" ]]; then
        echo "fixture: $dest already exists" >&2
        exit 1
    fi
    export HOME="$dest.home"
    mkdir -p "$HOME"
    g init -q "$dest"
    cd "$dest"
    tick
}

# commit MESSAGE: commits the index (allowing empty commits) at the next tick.
commit() {
    tick
    g commit -q --allow-empty -m "$1"
}

# write PATH CONTENT: writes a file (creating directories) and stages it.
write() {
    mkdir -p -- "$(dirname -- "$1")"
    printf '%s' "$2" >"$1"
    g add -- "$1"
}

fixture_done() {
    rm -rf "$HOME"
}
