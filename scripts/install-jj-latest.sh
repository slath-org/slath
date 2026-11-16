#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
# SPDX-License-Identifier: AGPL-3.0-or-later
#
# Installs the latest published Jujutsu release and prints its path. For the
# nightly compatibility matrix: a new jj release is tested without anyone
# pinning it first. The download is checked against the SHA-256 digest
# GitHub publishes for the release asset (pinned versions use
# scripts/install-jj.sh and the digests in tests/compat/versions.toml).
# Set GH_TOKEN to avoid the API's rate limit for anonymous requests.
# Usage: scripts/install-jj-latest.sh [prefix-dir]
# Default prefix-dir: ${SLATH_TOOLS_DIR:-~/.cache/slath/tools}

set -euo pipefail

dir="${1:-${SLATH_TOOLS_DIR:-$HOME/.cache/slath/tools}}"
arch="$(uname -m)"
case "$arch" in
    x86_64 | aarch64) ;;
    arm64) arch=aarch64 ;;
    *) echo "install-jj-latest: unsupported architecture $arch" >&2; exit 1 ;;
esac

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
auth=()
if [[ -n "${GH_TOKEN:-}" ]]; then
    auth=(-H "Authorization: Bearer $GH_TOKEN")
fi
curl --proto '=https' --tlsv1.2 -sSfL "${auth[@]}" -H "Accept: application/vnd.github+json" \
    -o "$tmp/release.json" https://api.github.com/repos/jj-vcs/jj/releases/latest

# version, asset name, asset URL, and digest, one per line
mapfile -t info < <(python3 - "$tmp/release.json" "$arch" <<'EOF'
import json, sys
release = json.load(open(sys.argv[1]))
name = f"{release['tag_name']}-{sys.argv[2]}-unknown-linux-musl.tar.gz"
name = "jj-" + name
asset = next((a for a in release["assets"] if a["name"] == name), None)
if asset is None or not (asset.get("digest") or "").startswith("sha256:"):
    sys.exit(f"install-jj-latest: no {name} with a SHA-256 digest in {release['tag_name']}")
print(release["tag_name"].removeprefix("v"))
print(name)
print(asset["browser_download_url"])
print(asset["digest"].removeprefix("sha256:"))
EOF
)
version="${info[0]}" asset="${info[1]}" url="${info[2]}" want="${info[3]}"
prefix="$dir/jj-$version"
if [[ -x "$prefix/bin/jj" ]]; then
    echo "$prefix/bin/jj"
    exit 0
fi

curl --proto '=https' --tlsv1.2 -sSfL -o "$tmp/$asset" "$url"
got="$(sha256sum "$tmp/$asset" | cut -d' ' -f1)"
if [[ "$got" != "$want" ]]; then
    echo "install-jj-latest: checksum mismatch for $asset: got $got, want $want" >&2
    exit 1
fi
mkdir -p "$tmp/x" "$prefix/bin"
tar -xzf "$tmp/$asset" -C "$tmp/x"
install -m 0755 "$(find "$tmp/x" -type f -name jj | head -1)" "$prefix/bin/jj"
echo "$prefix/bin/jj"
