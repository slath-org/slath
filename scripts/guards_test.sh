#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2026 Erich Seifert
# SPDX-License-Identifier: AGPL-3.0-or-later
#
# Tests for guards.sh: a clean tree passes, and each deliberate violation fails
# the guard it targets. Runs against throwaway trees, never the real one.

set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
GUARDS="$HERE/guards.sh"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT
passed=0
failed=0

HDR='// SPDX-FileCopyrightText: 2026 Test
// SPDX-License-Identifier: AGPL-3.0-or-later'

# Builds a minimal clean tree in $1.
clean_tree() {
    local t="$1"
    mkdir -p "$t"/{scripts,crates/git/src,crates/http/src,crates/http/templates,crates/sdk/src,web/src}
    printf '[workspace]\n\n[workspace.package]\nlicense = "AGPL-3.0-or-later"\n' >"$t/Cargo.toml"
    printf '[package]\nname = "x"\nlicense.workspace = true\n' >"$t/crates/http/Cargo.toml"
    printf '[package]\nname = "x"\nlicense.workspace = true\n' >"$t/crates/git/Cargo.toml"
    printf '[package]\nname = "sdk"\nlicense = "Apache-2.0"\n' >"$t/crates/sdk/Cargo.toml"
    printf '%s\nfn main() {}\n' "$HDR" >"$t/crates/http/src/lib.rs"
    printf '// SPDX-FileCopyrightText: 2026 Test\n// SPDX-License-Identifier: Apache-2.0\n' >"$t/crates/sdk/src/lib.rs"
    printf '%s\nuse tokio::process::Command;\nfn f() { Command::new("/usr/bin/git").args(["-c", "x=y"]); }\n' \
        "$HDR" >"$t/crates/git/src/runner.rs"
    printf '<a href="https://example.org/docs">docs</a>\n<img src="/-/assets/logo.svg">\n{{ name }}\n' \
        >"$t/crates/http/templates/base.html"
    printf '/* SPDX-License-Identifier: AGPL-3.0-or-later */\nbody { background: url("/-/assets/a.png"); }\n' \
        >"$t/web/src/app.css"
}

# expect <name> <pass|fail> <guard-or-empty> <setup-command>
expect() {
    local name="$1" want="$2" guard="$3" setup="$4"
    local t="$TMP/$name"
    clean_tree "$t"
    (cd "$t" && eval "$setup")
    local out status=0
    out="$("$GUARDS" "$t" 2>&1)" || status=$?
    if [[ "$want" == pass && $status -eq 0 ]] ||
        [[ "$want" == fail && $status -ne 0 && "$out" == *"guard $guard:"* ]]; then
        passed=$((passed + 1))
    else
        failed=$((failed + 1))
        echo "FAIL $name (want $want${guard:+ by $guard}, exit $status):" >&2
        echo "$out" | sed 's/^/    /' >&2
    fi
}

expect clean pass "" ":"
expect spdx-missing fail spdx \
    "printf 'fn x() {}\n' > crates/http/src/extra.rs"
expect spdx-wrong-license fail spdx \
    "printf '// SPDX-License-Identifier: Apache-2.0\n' > crates/http/src/extra.rs"
expect spdx-apache-crate-needs-apache fail spdx \
    "printf '%s\n' '// SPDX-License-Identifier: AGPL-3.0-or-later' > crates/sdk/src/lib.rs"
expect spdx-script fail spdx \
    "printf '#!/bin/sh\necho hi\n' > scripts/x.sh"
expect git-spawn fail git-spawn \
    "printf '%s\nfn f() { std::process::Command::new(\"git\"); }\n' '$HDR' > crates/http/src/spawn.rs"
expect git-spawn-allowlisted pass "" \
    "printf '%s\nfn f() { std::process::Command::new(\"git\"); }\n' '$HDR' > crates/http/src/spawn.rs
     printf 'git-spawn crates/http/src/spawn.rs  # test exception\n' > scripts/guards-allowlist.txt"
expect allowlist-needs-reason fail git-spawn \
    "printf '%s\nfn f() { std::process::Command::new(\"git\"); }\n' '$HDR' > crates/http/src/spawn.rs
     printf 'git-spawn crates/http/src/spawn.rs\n' > scripts/guards-allowlist.txt"
expect spawn-in-tests-ok pass "" \
    "mkdir -p crates/http/tests && printf '%s\nfn f() { std::process::Command::new(\"git\"); }\n' '$HDR' > crates/http/tests/t.rs"
expect shell-binary fail no-shell \
    "printf '%s\nconst S: &str = \"/bin/sh\";\n' '$HDR' > crates/http/src/sh.rs"
expect shell-bash fail no-shell \
    "printf '%s\nconst S: &str = \"bash\";\n' '$HDR' > crates/http/src/sh.rs"
expect shell-dash-c fail no-shell \
    "printf '%s\nconst C: &str = \"-c\";\n' '$HDR' > crates/http/src/sh.rs"
expect askama-safe fail askama-safe \
    "printf '{{ body|safe }}\n' > crates/http/templates/page.html"
expect askama-safe-spaced fail askama-safe \
    "printf '{{ body | safe }}\n' > crates/http/templates/page.html"
expect external-script fail external-url \
    "printf '<script src=\"https://cdn.example.org/x.js\"></script>\n' > crates/http/templates/page.html"
expect external-protocol-relative fail external-url \
    "printf '<img src=\"//cdn.example.org/x.png\">\n' > crates/http/templates/page.html"
expect external-stylesheet fail external-url \
    "printf '<link rel=\"stylesheet\" href=\"https://fonts.example.org/css\">\n' > crates/http/templates/page.html"
expect external-css-url fail external-url \
    "printf '/* SPDX-License-Identifier: AGPL-3.0-or-later */\na { background: url(https://x.org/a.png); }\n' > web/src/b.css"
expect external-css-import fail external-url \
    "printf '/* SPDX-License-Identifier: AGPL-3.0-or-later */\n@import \"https://x.org/a.css\";\n' > web/src/b.css"
expect external-fetch fail external-url \
    "printf '// SPDX-License-Identifier: AGPL-3.0-or-later\nfetch(\"https://x.org/api\");\n' > web/src/a.ts"
expect external-dist fail external-url \
    "mkdir -p web/dist && printf 'new Worker(\"https://x.org/w.js\")\n' > web/dist/app.js"
expect external-link-anchor-ok pass "" \
    "printf '<a href=\"https://example.org\">x</a>\n' > crates/http/templates/page.html"

echo "guards_test: $passed passed, $failed failed"
((failed == 0))
