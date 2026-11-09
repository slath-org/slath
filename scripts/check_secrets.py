#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Fails on complete token-shaped strings and private keys in the tree.

Code hosts scan pushes for credentials and stop them, even when they are
test samples. Samples are therefore kept split in the source (see
`secret_scan::joined`) and test keys encoded, so no full one is committed.
Placeholders (all `x`, or containing `EXAMPLE`) are fine.

Usage: check_secrets.py ROOT   Exit status 1 on violations. Exceptions:
`secret-literal <path>  # reason` in scripts/guards-allowlist.txt.
"""
import os
import re
import subprocess
import sys
from pathlib import Path

RULES = [
    ("GitHub token", r"\bgh[pousr]_([A-Za-z0-9]{36})"),
    ("GitHub fine-grained token", r"\bgithub_pat_([A-Za-z0-9_]{60,})"),
    ("GitLab token", r"\bglpat-([A-Za-z0-9_-]{20})"),
    ("AWS access key", r"\bAKIA([0-9A-Z]{16})\b"),
    ("Slack token", r"\bxox[baprs]-([0-9]{6,}-[A-Za-z0-9-]+)"),
    ("Stripe live key", r"\b[sr]k_live_([0-9A-Za-z]{20,})"),
    ("Google API key", r"\bAIza([0-9A-Za-z_-]{35})"),
    ("npm token", r"\bnpm_([A-Za-z0-9]{36})"),
    ("PyPI token", r"\bpypi-(AgE[A-Za-z0-9_-]{30,})"),
    # A key header followed by key material (a real newline or an escaped one).
    ("private key", r"-----BEGIN [A-Z ]*PRIVATE KEY-----(?:\n|\\n)([A-Za-z0-9+/=]{20,})"),
]
PRUNE = {".git", "target", "node_modules", "dist"}
MAX_BYTES = 2 * 1024 * 1024


def placeholder(body: str) -> bool:
    return len(set(body.lower())) <= 1 or "EXAMPLE" in body.upper()


def tracked(root: Path) -> list[Path]:
    if (root / ".git").exists():
        out = subprocess.run(["git", "-C", str(root), "ls-files", "-z"],
                             check=True, capture_output=True).stdout
        return [root / p for p in out.decode().split("\0") if p]
    files = []
    for d, dirs, names in os.walk(root):
        dirs[:] = [x for x in dirs if x not in PRUNE]
        files += [Path(d) / n for n in names]
    return files


def allowed(root: Path, rel: str) -> bool:
    allow = root / "scripts/guards-allowlist.txt"
    if not allow.exists():
        return False
    pat = re.compile(rf"^secret-literal\s+{re.escape(rel)}\s+#\s*\S")
    return any(pat.match(l) for l in allow.read_text(encoding="utf-8").splitlines())


def main(argv: list[str]) -> int:
    root = Path(argv[0]).resolve()
    rules = [(name, re.compile(rx)) for name, rx in RULES]
    bad = 0
    for f in tracked(root):
        if not f.is_file() or f.stat().st_size > MAX_BYTES:
            continue
        data = f.read_bytes()
        if b"\0" in data[:8000]:
            continue
        text = data.decode("utf-8", "replace")
        rel = f.relative_to(root).as_posix()
        for name, rx in rules:
            for m in rx.finditer(text):
                if placeholder(m.group(1)) or allowed(root, rel):
                    continue
                line = text.count("\n", 0, m.start()) + 1
                print(f"{rel}:{line}: complete {name} in the tree; split it (secret_scan::joined) or encode it",
                      file=sys.stderr)
                bad += 1
    return 1 if bad else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:] or ["."]))
