#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
# SPDX-License-Identifier: AGPL-3.0-or-later
# Lightweight, annotated, and nested tags, including a tag of a tag and of a tree.
source "$(dirname "$0")/lib.sh"
fixture_init "$1"

write a.txt $'1\n'
commit "v1"
g tag v1.0-light
tick
g tag -a -m "Release 1.0" v1.0
write a.txt $'2\n'
commit "v2"
tick
g tag -a -m "Release 2.0" release/2.0
tick
g tag -a -m "Tag of a tag" v2.0-meta release/2.0
tick
g tag -a -m "Tag of a tree" tree-tag "HEAD^{tree}"
fixture_done
