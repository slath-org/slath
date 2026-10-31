#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
# SPDX-License-Identifier: AGPL-3.0-or-later
# A long linear history (default 10 000 commits) built with fast-import.
# Usage: make_deep_history.sh DEST [COMMITS]
source "$(dirname "$0")/lib.sh"
n="${2:-10000}"
fixture_init "$1"

python3 - "$n" <<'PY' | g fast-import --quiet
import sys
n = int(sys.argv[1])
out = sys.stdout.buffer
for i in range(1, n + 1):
    t = 1700000000 + 60 * i
    data = f"line {i}\n".encode()
    msg = f"Commit {i}\n".encode()
    out.write(b"commit refs/heads/main\n")
    out.write(f"mark :{i}\n".encode())
    out.write(f"author Ada Author <ada@example.org> {t} +0000\n".encode())
    out.write(f"committer Cy Committer <cy@example.org> {t} +0000\n".encode())
    out.write(f"data {len(msg)}\n".encode() + msg)
    if i > 1:
        out.write(f"from :{i - 1}\n".encode())
    out.write(f"M 100644 inline counter.txt\ndata {len(data)}\n".encode() + data + b"\n")
PY
g reset -q --hard main
fixture_done
