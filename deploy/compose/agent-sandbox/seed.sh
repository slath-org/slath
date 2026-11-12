#!/bin/sh
# SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
# SPDX-License-Identifier: AGPL-3.0-or-later
#
# Applies seed.toml as the slath user (repositories belong to it), with the
# secrets of earlier runs, then copies the secrets to /secrets, owned like
# that directory (the person who started the sandbox).
set -eu
work=$(mktemp -d)
mkdir -p "$work/secrets"
cp /sandbox/seed.toml "$work/"
for f in /secrets/*; do
    [ -f "$f" ] && cp "$f" "$work/secrets/"
done
chown -R slath:slath "$work"
chmod 700 "$work/secrets"
setpriv --reuid=slath --regid=slath --init-groups slath admin apply "$work/seed.toml"
owner=$(stat -c %u:%g /secrets)
for f in "$work"/secrets/*; do
    [ -f "$f" ] && install -m 600 -o "${owner%:*}" -g "${owner#*:}" "$f" "/secrets/$(basename "$f")"
done
rm -rf "$work"
port=${SLATH__GIT__SSH_PORT:-2280}
url=${SLATH__SERVER__PUBLIC_URL:-http://127.0.0.1:3080}
cat <<MSG

Slath agent sandbox (not for production) at $url
  Sign in as ada; the password is in secrets/ada-password.
  The bot "agent" can write to sandbox/playground with the token in
  secrets/bot-token:
    git clone "http://agent:\$(cat secrets/bot-token)@${url#http://}/sandbox/playground.git"
  API: curl -H "Authorization: Bearer \$(cat secrets/bot-token)" $url/api/v1/user
  SSH (after adding a key to the bot): ssh://git@127.0.0.1:$port/sandbox/playground.git
MSG
