#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
# SPDX-License-Identifier: AGPL-3.0-or-later
#
# Installs the newest stable Git on an Ubuntu CI runner from the Git
# maintainers' PPA. Adding the PPA asks Launchpad, which is sometimes
# briefly unavailable (HTTP 503), so each step is tried up to three
# times, 30 seconds apart, before the job fails.

set -euo pipefail

retry() {
    local n
    for n in 1 2 3; do
        "$@" && return 0
        if [[ $n -lt 3 ]]; then
            echo "::warning::'$*' failed (try $n of 3); trying again in 30 s" >&2
            sleep 30
        fi
    done
    echo "::error::'$*' failed three times" >&2
    return 1
}

retry sudo add-apt-repository -y ppa:git-core/ppa
retry sudo apt-get install -y --no-install-recommends git
git --version
