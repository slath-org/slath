#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Prints the releases the upgrade matrix upgrades from, oldest first.

Reads tags (vX.Y.Z or vX.Y.Z-rc.N) on stdin, one per line. The sources
are every release since the upgrade baseline 0.3.0 (the 0.1/0.2 previews
can't be upgraded), and the release candidates newer than the newest
release: those of the version being prepared, which update to it. Older
candidates were replaced by their release. Anything else (other tags, the
--exclude tag being built) is left out.

Usage: git tag --list | upgrade_sources.py [--exclude TAG]
"""
from __future__ import annotations

import argparse
import re
import sys

TAG = re.compile(r"^v(\d+)\.(\d+)\.(\d+)(?:-rc\.(\d+))?$")


def key(tag: str) -> tuple:
    m = TAG.match(tag)
    assert m
    major, minor, patch, rc = m.groups()
    # A release sorts after its candidates.
    return (int(major), int(minor), int(patch), int(rc) if rc else float("inf"))


def sources(tags: list[str], exclude: str = "") -> list[str]:
    tags = sorted({t for t in tags if TAG.match(t) and t != exclude}, key=key)
    tags = [t for t in tags if key(t)[:3] >= (0, 3, 0)]
    releases = [t for t in tags if "-" not in t]
    newest = key(releases[-1]) if releases else (0, 0, 0, 0)
    return [t for t in tags if "-" not in t or key(t) > newest]


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--exclude", default="")
    args = ap.parse_args()
    for t in sources(sys.stdin.read().split(), args.exclude):
        print(t)
    return 0


if __name__ == "__main__":
    sys.exit(main())
