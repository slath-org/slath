#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
# SPDX-License-Identifier: AGPL-3.0-or-later
#
# Installs a pinned Jujutsu release binary and prints its path.
# Usage: scripts/install-jj.sh <version> [prefix]
# Default prefix: ${SLATH_TOOLS_DIR:-~/.cache/slath/tools}/jj-<version>

set -euo pipefail

version="${1:?usage: install-jj.sh <version> [prefix]}"
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
prefix="${2:-${SLATH_TOOLS_DIR:-$HOME/.cache/slath/tools}/jj-$version}"
arch="$(uname -m)"
case "$arch" in
    x86_64 | aarch64) ;;
    arm64) arch=aarch64 ;;
    *) echo "install-jj: unsupported architecture $arch" >&2; exit 1 ;;
esac

if [[ -x "$prefix/bin/jj" ]]; then
    echo "$prefix/bin/jj"
    exit 0
fi

want="$(python3 "$here/pinned_sha256.py" jj "$version" "$arch")"
asset="jj-v$version-$arch-unknown-linux-musl.tar.gz"
url="https://github.com/jj-vcs/jj/releases/download/v$version/$asset"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

curl --proto '=https' --tlsv1.2 -sSfL -o "$tmp/$asset" "$url"
got="$(sha256sum "$tmp/$asset" | cut -d' ' -f1)"
if [[ "$got" != "$want" ]]; then
    echo "install-jj: checksum mismatch for $asset: got $got, want $want" >&2
    exit 1
fi
mkdir -p "$tmp/x" "$prefix/bin"
tar -xzf "$tmp/$asset" -C "$tmp/x"
install -m 0755 "$(find "$tmp/x" -type f -name jj | head -1)" "$prefix/bin/jj"
echo "$prefix/bin/jj"
