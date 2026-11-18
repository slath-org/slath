#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
# SPDX-License-Identifier: AGPL-3.0-or-later
#
# Builds a pinned Git release from source and prints the path of its binary.
# Needs a C toolchain plus zlib, curl, and expat development headers. Builds
# as C17 because older releases clash with C23 headers (`unreachable`).
# Usage: scripts/install-git.sh <version> [prefix]
# Default prefix: ${SLATH_TOOLS_DIR:-~/.cache/slath/tools}/git-<version>

set -euo pipefail

version="${1:?usage: install-git.sh <version> [prefix]}"
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
prefix="${2:-${SLATH_TOOLS_DIR:-$HOME/.cache/slath/tools}/git-$version}"

if [[ -x "$prefix/bin/git" ]]; then
    echo "$prefix/bin/git"
    exit 0
fi

want="$(python3 "$here/pinned_sha256.py" git "$version")"
tarball="git-$version.tar.xz"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

# Several kernel.org hosts, each tried with retries, so one unreachable
# host doesn't fail a build. The checksum pins the file whichever serves it.
fetched=""
for host in mirrors.edge.kernel.org cdn.kernel.org www.kernel.org; do
    url="https://$host/pub/software/scm/git/$tarball"
    if curl --proto '=https' --tlsv1.2 -sSfL --connect-timeout 20 --retry 3 --retry-all-errors \
        -o "$tmp/$tarball" "$url"; then
        fetched=1
        break
    fi
    echo "install-git: $url failed; trying the next host" >&2
done
if [[ -z "$fetched" ]]; then
    echo "install-git: no host served $tarball" >&2
    exit 1
fi
got="$(sha256sum "$tmp/$tarball" | cut -d' ' -f1)"
if [[ "$got" != "$want" ]]; then
    echo "install-git: checksum mismatch for $tarball: got $got, want $want" >&2
    exit 1
fi
tar -xJf "$tmp/$tarball" -C "$tmp"
make -C "$tmp/git-$version" -j"$(nproc)" prefix="$prefix" \
    CFLAGS="-O2 -std=gnu17" NO_GETTEXT=1 NO_TCLTK=1 NO_PERL=1 NO_PYTHON=1 INSTALL_SYMLINKS=1 \
    all install >"$tmp/build.log" 2>&1 || {
    tail -40 "$tmp/build.log" >&2
    exit 1
}
echo "$prefix/bin/git"
