#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
# SPDX-License-Identifier: AGPL-3.0-or-later
# Merge commits: a two-parent merge, an octopus merge, and a multi-author history.
source "$(dirname "$0")/lib.sh"
fixture_init "$1"

write base.txt $'base\n'
commit "Base"
for b in one two three; do
    g checkout -q -b "$b" main
    write "$b.txt" "$b"$'\n'
    GIT_AUTHOR_NAME="Author $b" GIT_AUTHOR_EMAIL="$b@example.org" commit "Work on $b"
done
g checkout -q main
write main.txt $'main\n'
commit "Work on main"
tick
g merge -q --no-ff -m "Merge branch 'one'" one
tick
g merge -q --no-ff -m "Octopus merge" two three
fixture_done
