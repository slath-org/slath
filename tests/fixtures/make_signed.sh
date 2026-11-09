#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
# SPDX-License-Identifier: AGPL-3.0-or-later
# SSH-signed commits and tags (gpgsig headers) using the public test key.
source "$(dirname "$0")/lib.sh"
key="$FIXTURES_DIR/keys/signing_ed25519.b64"
fixture_init "$1"
# Stored base64-encoded so no private key is committed as such (code hosts
# reject pushes containing one). ssh-keygen refuses keys others can read.
(umask 077 && base64 -d "$key" >"$HOME/key")
sign=(-c gpg.format=ssh -c user.signingKey="$HOME/key")

write signed.txt $'signed\n'
tick
g "${sign[@]}" commit -q -S -m "Signed commit"
write unsigned.txt $'unsigned\n'
commit "Unsigned commit"
tick
g "${sign[@]}" tag -s -m "Signed tag" v1.0-signed
fixture_done
