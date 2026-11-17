#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Adds one version's documentation to the website's docs directory.

Run by the website repository's workflow on the unpacked
slath-docs-<version>.tar.gz of a release (`slath docs build
--published`). The site goes to DOCS/<version>/; DOCS/versions.json lists every published
version, newest first; DOCS/versions.html links them (the version
switcher's fallback without JavaScript); DOCS/latest/ is a copy of the
newest release that isn't a pre-release, and DOCS/index.html leads there.

Usage: publish_docs.py --docs DOCS --site SITE --version X.Y.Z
"""
from __future__ import annotations

import argparse
import html
import json
import re
import shutil
import sys
from pathlib import Path

VERSION = re.compile(r"^\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?$")


def key(v: str) -> tuple:
    core, _, pre = v.partition("-")
    nums = tuple(int(x) for x in core.split("."))
    # A release sorts after its pre-releases.
    return nums + ((1, "") if not pre else (0, pre))


def page(title: str, body: str) -> str:
    return f"""<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<meta name="color-scheme" content="light dark">
<title>{html.escape(title)}</title>
</head>
<body>
{body}
</body>
</html>
"""


def publish(docs: Path, site: Path, version: str) -> list[str]:
    if not VERSION.match(version):
        raise ValueError(f"not a version: {version!r}")
    if not (site / "index.html").is_file():
        raise ValueError(f"{site} has no index.html")
    target = docs / version
    if target.exists():
        shutil.rmtree(target)
    shutil.copytree(site, target)
    versions = sorted(
        (p.name for p in docs.iterdir() if p.is_dir() and VERSION.match(p.name)),
        key=key,
        reverse=True,
    )
    (docs / "versions.json").write_text(json.dumps({"versions": versions}, indent=2) + "\n")
    items = "\n".join(
        f'<li><a href="{html.escape(v)}/index.html">Slath {html.escape(v)}</a></li>' for v in versions
    )
    (docs / "versions.html").write_text(
        page("Slath documentation: versions", f"<main>\n<h1>Slath documentation</h1>\n<ul>\n{items}\n</ul>\n</main>")
    )
    releases = [v for v in versions if "-" not in v]
    if releases:
        latest = docs / "latest"
        if latest.exists():
            shutil.rmtree(latest)
        shutil.copytree(docs / releases[0], latest)
        (docs / "index.html").write_text(
            page(
                "Slath documentation",
                '<main>\n<h1>Slath documentation</h1>\n<p><a href="latest/index.html">The latest release</a> · '
                '<a href="versions.html">All versions</a></p>\n</main>',
            )
        )
    return versions


def main(argv: list[str]) -> int:
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("--docs", required=True, type=Path)
    p.add_argument("--site", required=True, type=Path)
    p.add_argument("--version", required=True)
    a = p.parse_args(argv)
    a.docs.mkdir(parents=True, exist_ok=True)
    try:
        versions = publish(a.docs, a.site, a.version)
    except ValueError as e:
        print(f"publish_docs: {e}", file=sys.stderr)
        return 1
    print("publish_docs: " + ", ".join(versions))
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
