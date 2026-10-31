#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
# SPDX-License-Identifier: AGPL-3.0-or-later
# Basic linear history on `main` plus a `feature` branch, with nested paths and a rename.
source "$(dirname "$0")/lib.sh"
fixture_init "$1"

write README.md $'# Basic fixture\n'
commit "Initial commit"
write src/lib.rs $'pub fn answer() -> u32 { 42 }\n'
write src/deep/nested/dir/file.txt $'nested\n'
commit "Add sources"
g mv src/lib.rs src/main.rs
commit "Rename lib.rs to main.rs"
g checkout -q -b feature
write feature.txt $'feature work\n'
commit "Add feature"
g checkout -q main
write README.md $'# Basic fixture\n\nMore text.\n'
commit "Update README"
fixture_done
