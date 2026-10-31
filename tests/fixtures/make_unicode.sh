#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
# SPDX-License-Identifier: AGPL-3.0-or-later
# Unicode and unusual paths, a Unicode commit message, and a non-UTF-8 branch name.
source "$(dirname "$0")/lib.sh"
fixture_init "$1"

write "ünïcödé/naïve café.txt" $'NFC name\n'
write "$(printf 'nfd/cafe\xcc\x81.txt')" $'NFD name\n'
write "日本語/ファイル.md" $'# 日本語\n'
write "emoji/🦀.rs" $'// crab\n'
write "spaces and 'quotes'/a \"b\".txt" $'quoted\n'
write "-leading-dash.txt" $'dash\n'
write "$(printf 'latin1-\xe9.txt')" $'non-UTF-8 file name\n'
commit "Ünïcödé paths — 日本語 🦀"
tick
g branch "$(printf 'caf\xc3\xa9')"
# A ref name that is valid for Git but not UTF-8.
g update-ref "$(printf 'refs/heads/latin1-\xe9')" HEAD
fixture_done
