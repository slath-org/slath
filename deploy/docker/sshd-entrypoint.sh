#!/bin/sh
# SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
# SPDX-License-Identifier: AGPL-3.0-or-later
#
# Runs sshd for Git over SSH in the Slath image. Host keys persist in
# /etc/ssh/keys (mount a volume there); they're generated on first start.
set -eu

keys=/etc/ssh/keys
mkdir -p "$keys"
for type in ed25519 ecdsa rsa; do
    if [ ! -f "$keys/ssh_host_${type}_key" ]; then
        ssh-keygen -q -t "$type" -N "" -f "$keys/ssh_host_${type}_key"
    fi
    chmod 0600 "$keys/ssh_host_${type}_key"
done
cat >/etc/ssh/sshd_config.d/00-host-keys.conf <<CONF
HostKey $keys/ssh_host_ed25519_key
HostKey $keys/ssh_host_ecdsa_key
HostKey $keys/ssh_host_rsa_key
CONF
exec /usr/sbin/sshd -D -e "$@"
