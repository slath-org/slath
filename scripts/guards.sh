#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2026 Erich Seifert
# SPDX-License-Identifier: AGPL-3.0-or-later
#
# Static guards for rules that reviewers would otherwise have to catch by eye
# (security hardening checklist).
#
#   spdx          every source file starts with the SPDX header for its crate
#   git-spawn     `Command::new` only in crates/git/src/runner.rs
#   no-shell      no shell binaries or `-c` in server code
#   askama-safe   no `|safe` filter in templates unless allowlisted
#   external-url  no third-party URLs in resource positions
#
# Usage: scripts/guards.sh [ROOT]   (ROOT defaults to the repository root)
# Exceptions: scripts/guards-allowlist.txt, one `<guard> <path>  # reason` per line.

set -euo pipefail

ROOT="${1:-$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)}"
cd "$ROOT"
ALLOWLIST="scripts/guards-allowlist.txt"
failures=0

fail() {
    echo "guard $1: $2" >&2
    failures=$((failures + 1))
}

allowed() { # guard path
    [[ -f "$ALLOWLIST" ]] || return 1
    grep -Eq "^$1[[:space:]]+$2[[:space:]]+#[[:space:]]*[^[:space:]]" "$ALLOWLIST"
}

# Lists files under the given directories (if they exist), excluding build output.
files() { # pattern dir...
    local pattern="$1"
    shift
    local dirs=()
    for d in "$@"; do [[ -e "$d" ]] && dirs+=("$d"); done
    ((${#dirs[@]})) || return 0
    find "${dirs[@]}" \( -name target -o -name node_modules -o -name .git \) -prune -o \
        -type f -name "$pattern" -print | sort
}

# The license a Rust file must declare: from the nearest Cargo.toml.
expected_license() { # file
    local dir
    dir="$(dirname "$1")"
    while [[ "$dir" != "." && "$dir" != "/" ]]; do
        if [[ -f "$dir/Cargo.toml" ]]; then
            if grep -Eq '^license\.workspace[[:space:]]*=[[:space:]]*true' "$dir/Cargo.toml"; then
                sed -nE 's/^license[[:space:]]*=[[:space:]]*"([^"]+)".*/\1/p' Cargo.toml | head -1
            else
                sed -nE 's/^license[[:space:]]*=[[:space:]]*"([^"]+)".*/\1/p' "$dir/Cargo.toml" | head -1
            fi
            return
        fi
        dir="$(dirname "$dir")"
    done
    sed -nE 's/^license[[:space:]]*=[[:space:]]*"([^"]+)".*/\1/p' Cargo.toml | head -1
}

# --- spdx --------------------------------------------------------------------
while IFS= read -r f; do
    allowed spdx "$f" && continue
    want="$(expected_license "$f")"
    if ! head -n 3 "$f" | grep -Fqx "// SPDX-License-Identifier: $want"; then
        fail spdx "$f must start with '// SPDX-License-Identifier: $want'"
    fi
done < <(files '*.rs' crates apps tests)

while IFS= read -r f; do
    allowed spdx "$f" && continue
    if ! head -n 3 "$f" | grep -Eq 'SPDX-License-Identifier: [A-Za-z0-9.+-]+'; then
        fail spdx "$f has no SPDX-License-Identifier header in its first 3 lines"
    fi
done < <(
    files '*.sh' scripts tests deploy
    files '*.py' scripts tests
    files '*.ts' web/src tests/e2e
    files '*.css' web/src
)

# Server code: Rust sources outside tests and benches.
server_sources() {
    files '*.rs' crates apps | grep -Ev '/(tests|benches)/' || true
}

# --- git-spawn ---------------------------------------------------------------
while IFS= read -r f; do
    [[ "$f" == "crates/git/src/runner.rs" ]] && continue
    allowed git-spawn "$f" && continue
    if grep -nE 'Command::new|process::Command|std::os::unix::process::CommandExt' "$f" >/dev/null; then
        fail git-spawn "$f spawns processes; only crates/git/src/runner.rs may"
    fi
done < <(server_sources)

# --- no-shell ----------------------------------------------------------------
while IFS= read -r f; do
    allowed no-shell "$f" && continue
    if grep -nE '"(/usr)?(/bin/)?(ba|da|z|k|c|tc|fi)?sh"' "$f" >/dev/null; then
        fail no-shell "$f references a shell binary"
    fi
    if [[ "$f" != "crates/git/src/runner.rs" ]] && grep -nF '"-c"' "$f" >/dev/null; then
        fail no-shell "$f passes \"-c\" (shell command strings are forbidden)"
    fi
done < <(server_sources)

# --- askama-safe -------------------------------------------------------------
while IFS= read -r f; do
    allowed askama-safe "$f" && continue
    if grep -nE '\|[[:space:]]*safe\b' "$f" >/dev/null; then
        fail askama-safe "$f uses the |safe filter"
    fi
done < <(files '*' crates/http/templates)

# --- external-url --------------------------------------------------------------
# Resource-loading positions: src/srcset/poster/data attributes, <link href>,
# CSS url() and @import, fetch/import()/Worker/EventSource/WebSocket targets.
ext='(https?:)?//'
resource_re="(\b(src|srcset|poster|data)[[:space:]]*=[[:space:]]*[\"']?$ext)"
resource_re+="|(<link\b[^>]*\bhref[[:space:]]*=[[:space:]]*[\"']?$ext)"
resource_re+="|(url\([[:space:]]*[\"']?$ext)"
resource_re+="|(@import[[:space:]]+(url\()?[[:space:]]*[\"']?$ext)"
resource_re+="|(\b(fetch|import|Worker|SharedWorker|EventSource|WebSocket|importScripts)[[:space:]]*\([[:space:]]*[\"'\`]($ext|wss?://))"
while IFS= read -r f; do
    allowed external-url "$f" && continue
    if grep -nEi "$resource_re" "$f" >/dev/null; then
        fail external-url "$f loads a resource from an absolute or protocol-relative URL"
    fi
done < <(files '*' crates/http/templates web/src web/dist)

if ((failures)); then
    echo "guards: $failures violation(s)" >&2
    exit 1
fi
echo "guards: ok"
