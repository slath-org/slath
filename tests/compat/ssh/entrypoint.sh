#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
# SPDX-License-Identifier: AGPL-3.0-or-later
# sshd refuses an AuthorizedKeysCommand that isn't root-owned, so install the
# mounted test binary as root before starting.
set -euo pipefail
install -o root -g root -m 0755 /mnt/slath /usr/bin/slath
ssh-keygen -q -t ed25519 -N '' -f /etc/ssh/ssh_host_ed25519_key
exec /usr/sbin/sshd -D -e
