#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
# SPDX-License-Identifier: AGPL-3.0-or-later
# A gitlink (submodule) entry. The submodule commit is synthesized, so no
# network or second repository is needed and the OIDs stay fixed.
source "$(dirname "$0")/lib.sh"
fixture_init "$1"

write README.md $'# Superproject\n'
commit "Initial commit"
tree="$(g mktree </dev/null)"
sub="$(g commit-tree -m "Submodule commit" "$tree")"
printf '[submodule "vendor/lib"]\n\tpath = vendor/lib\n\turl = https://example.invalid/lib.git\n' >.gitmodules
g add .gitmodules
g update-index --add --cacheinfo "160000,$sub,vendor/lib"
commit "Add submodule vendor/lib"
fixture_done
