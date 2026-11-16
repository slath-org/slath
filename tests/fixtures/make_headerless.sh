#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
# SPDX-License-Identifier: AGPL-3.0-or-later
# Commits without a change-id header in shapes Git itself rarely writes,
# for comparing Slath's predicted jj IDs with jj's: an unknown header, an
# encoding header, no message, a message without a final newline, a
# Change-Id trailer, an empty tree, a parentless commit beside main, and a
# merge whose parents repeat.
source "$(dirname "$0")/lib.sh"
fixture_init "$1"

write a.txt $'a\n'
commit "Base"
tree="$(g rev-parse 'HEAD^{tree}')"
empty="$(g hash-object -t tree -w /dev/null)"

# raw REF CONTENT: writes a commit object verbatim and points REF at it.
raw() {
    local oid
    oid="$(printf '%s' "$2" | g hash-object -t commit -w --literally --stdin)"
    g update-ref "$1" "$oid"
}

who="Ada Author <ada@example.org> 1700001000 +0000"
base="$(g rev-parse HEAD)"
raw refs/heads/unknown-header "tree $tree
parent $base
author $who
committer $who
x-tool slath-fixture

Unknown header
"
raw refs/heads/encoding "tree $tree
parent $base
author $who
committer $who
encoding ISO-8859-1

Encoding header
"
raw refs/heads/no-message "tree $tree
parent $base
author $who
committer $who

"
raw refs/heads/no-final-newline "tree $tree
parent $base
author $who
committer $who

No final newline"
raw refs/heads/gerrit-trailer "tree $tree
parent $base
author $who
committer $who

With a trailer

Change-Id: I0123456789abcdef0123456789abcdef01234567
"
raw refs/heads/empty-tree "tree $empty
parent $base
author $who
committer $who

Empty tree
"
raw refs/heads/second-root "tree $tree
author $who
committer $who

A second root
"
raw refs/heads/repeated-parent "tree $tree
parent $base
parent $base
author $who
committer $who

Repeated parent
"
fixture_done
