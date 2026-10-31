#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
# SPDX-License-Identifier: AGPL-3.0-or-later
# Binary blobs, symlinks (including a dangling one), and executable files.
source "$(dirname "$0")/lib.sh"
fixture_init "$1"

mkdir -p bin data
# 64 KiB of deterministic pseudo-random bytes plus a file with NUL bytes.
python3 -c 'import random,sys; r=random.Random(42); sys.stdout.buffer.write(bytes(r.getrandbits(8) for _ in range(65536)))' >data/random.bin
printf 'a\0b\0c\n' >data/nul.bin
printf '\x89PNG\r\n\x1a\n\0\0\0\rIHDR' >data/header.png
printf '#!/usr/bin/env bash\necho run\n' >bin/run.sh
chmod 0755 bin/run.sh
ln -s ../data/random.bin bin/random-link
ln -s does/not/exist dangling-link
g add -A
commit "Add binaries, symlinks, and an executable"
chmod 0644 bin/run.sh
g add bin/run.sh
commit "Drop executable bit"
fixture_done
