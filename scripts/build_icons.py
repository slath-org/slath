#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Builds the icon sprite (web/public/icons.svg) from the Lucide icons named
in web/src/icons/icons.txt, taken from the pinned lucide-static package.

Each icon becomes <symbol id="NAME" viewBox="0 0 24 24"> with Lucide's
stroke attributes, used as <svg><use href=".../icons.svg#NAME"></use></svg>.
The output is deterministic (sorted, no timestamps).

Usage: build_icons.py [ROOT]
"""
import re
import sys
from pathlib import Path

ATTRS = 'fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"'


def names(root: Path) -> list[str]:
    lines = (root / "web/src/icons/icons.txt").read_text(encoding="utf-8").splitlines()
    out = [l.strip() for l in lines if l.strip() and not l.lstrip().startswith("#")]
    if out != sorted(set(out)):
        raise SystemExit("icons: icons.txt must be sorted and free of duplicates")
    return out


def symbol(name: str, svg: str) -> str:
    body = re.search(r"<svg[^>]*>(.*)</svg>", svg, re.S)
    if not body:
        raise SystemExit(f"icons: cannot parse {name}.svg")
    inner = " ".join(line.strip() for line in body.group(1).strip().splitlines())
    if re.search(r"<script|on\w+=|href=", inner, re.I):
        raise SystemExit(f"icons: {name}.svg contains active content")
    return f'<symbol id="{name}" viewBox="0 0 24 24" {ATTRS}>{inner}</symbol>'


def main(argv: list[str]) -> int:
    root = Path(argv[0]) if argv else Path(__file__).resolve().parent.parent
    src = root / "web/node_modules/lucide-static/icons"
    symbols = []
    for name in names(root):
        f = src / f"{name}.svg"
        if not f.is_file():
            raise SystemExit(f"icons: unknown Lucide icon {name!r}")
        symbols.append(symbol(name, f.read_text(encoding="utf-8")))
    version = re.search(r'"version":\s*"([^"]+)"', (src.parent / "package.json").read_text()).group(1)
    out = root / "web/public/icons.svg"
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text(
        f'<svg xmlns="http://www.w3.org/2000/svg"><!-- Lucide {version} (ISC), see THIRD-PARTY-NOTICES.md -->\n'
        + "\n".join(symbols) + "\n</svg>\n",
        encoding="utf-8",
    )
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
