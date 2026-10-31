#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Print the pinned SHA-256 for a tool version from tests/compat/versions.toml.

Usage: pinned_sha256.py <git|jj> <version> [arch]
"""
import sys
import tomllib
from pathlib import Path

VERSIONS = Path(__file__).resolve().parent.parent / "tests" / "compat" / "versions.toml"


def main(argv: list[str]) -> int:
    if len(argv) not in (2, 3):
        print(__doc__, file=sys.stderr)
        return 2
    tool, version = argv[0], argv[1]
    arch = argv[2] if len(argv) == 3 else None
    data = tomllib.loads(VERSIONS.read_text())
    for entry in data.get(tool, []):
        if entry["version"] == version:
            sha = entry["sha256"]
            if isinstance(sha, dict):
                if arch not in sha:
                    print(f"no {tool} {version} pin for arch {arch!r}", file=sys.stderr)
                    return 1
                sha = sha[arch]
            print(sha)
            return 0
    print(f"{tool} {version} is not pinned in {VERSIONS}", file=sys.stderr)
    return 1


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
