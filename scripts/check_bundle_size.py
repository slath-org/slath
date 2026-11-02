#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Checks the Markdown editor's download size per mode.

Each editor mode (rich text: wysiwyg.js, source: source.js) is loaded with
shell.js and the chunks they import. Together they must stay within 150 KiB
gzip (level 9). The main bundle app.js must not contain editor code, so pages
without an editor don't pay for it.
"""

import gzip
import re
import sys
from pathlib import Path

BUDGET = 150 * 1024
MODES = ("wysiwyg.js", "source.js")
ENTRY = "shell.js"
EDITOR_MARKERS = ("prosemirror", "ProseMirror", "@codemirror", "cm-editor", "micromark")
IMPORT = re.compile(r"""(?:from|import)\s*\(?\s*["']\./([\w.-]+\.js)["']""")


def closure(dist: Path, start: str) -> set[str]:
    """`start` and every chunk it imports statically."""
    seen: set[str] = set()
    todo = [start]
    while todo:
        name = todo.pop()
        if name in seen or not (dist / name).is_file():
            continue
        seen.add(name)
        text = (dist / name).read_text(encoding="utf-8", errors="replace")
        # Static imports only; dynamic import("./x.js") loads later.
        for m in IMPORT.finditer(text):
            before = text[max(0, m.start() - 1) : m.start() + 6]
            if "import(" in text[m.start() : m.end()] or before.startswith("import("):
                continue
            todo.append(m.group(1))
    return seen


def gz(path: Path) -> int:
    return len(gzip.compress(path.read_bytes(), compresslevel=9, mtime=0))


def check(dist: Path) -> list[str]:
    errors = []
    if not (dist / ENTRY).is_file():
        return [f"{dist}/{ENTRY} is missing; build the web bundle first"]
    for mode in MODES:
        # app.js is the page's main bundle, loaded on every page anyway.
        files = (closure(dist, ENTRY) | closure(dist, mode)) - {"app.js"}
        total = sum(gz(dist / f) for f in files)
        print(f"bundle-size: {mode}: {total / 1024:.1f} KiB gzip ({', '.join(sorted(files))})")
        if total > BUDGET:
            errors.append(f"{mode} mode is {total / 1024:.1f} KiB gzip, over the {BUDGET // 1024} KiB budget")
    app = (dist / "app.js").read_text(encoding="utf-8", errors="replace")
    for marker in EDITOR_MARKERS:
        if marker in app:
            errors.append(f"app.js contains editor code ({marker}); it must load on demand")
    return errors


def main(argv: list[str]) -> int:
    dist = Path(argv[0]) if argv else Path(__file__).resolve().parent.parent / "web/dist"
    errors = check(dist)
    for e in errors:
        print(f"bundle-size: {e}", file=sys.stderr)
    return 1 if errors else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
