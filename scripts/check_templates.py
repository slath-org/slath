#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
# SPDX-License-Identifier: AGPL-3.0-or-later
"""htmx rules for templates and scripts (part of scripts/guards.sh).

Templates (crates/http/templates):
  - every start tag with an hx-* attribute (other than hx-ignore) must carry
    the nonce macro: {% call m::hx(...) %}{% endcall %}
  - forbidden: hx-on* attributes, hx-live, js:/javascript: in hx-vals or
    hx-confirm, trigger filters ([...] in hx-trigger), safeEval, a prefix
    setting in htmx-config
Scripts (web/src):
  - forbidden: hx-on, safeEval, setting htmx.config.prefix, eval(), new Function

Usage: check_templates.py ROOT   Exit status 1 on violations.
"""
import re
import sys
from pathlib import Path

TAG = re.compile(r"<[a-zA-Z][a-zA-Z0-9-]*\b(?:[^<>]|\{[%{].*?[%}]\})*>", re.S)
HX_ATTR = re.compile(r"(?<=\s)(?:data-)?hx-([a-z][a-z0-9:-]*)(?=[\s=/>])")
STAMP = re.compile(r"\{%-?\s*call\s+(?:\w+::)?hx\(")
ATTR_VALUE = re.compile(r"""(?<=\s)(hx-[a-z-]+)\s*=\s*(?:"([^"]*)"|'([^']*)')""")


def check_template(path: Path, text: str) -> list[str]:
    errors = []
    if "safeEval" in text:
        errors.append(f"{path}: safeEval is forbidden")
    for meta in re.finditer(r"""name=["']htmx-config["'][^>]*""", text):
        if "prefix" in meta.group(0):
            errors.append(f"{path}: htmx-config must not set prefix")
    for tag in TAG.finditer(text):
        t = tag.group(0)
        line = text.count("\n", 0, tag.start()) + 1
        attrs = {m.group(1) for m in HX_ATTR.finditer(t)}
        for a in attrs:
            if a.startswith("on"):
                errors.append(f"{path}:{line}: hx-on attributes are forbidden")
            if a == "live":
                errors.append(f"{path}:{line}: hx-live is forbidden")
        for m in ATTR_VALUE.finditer(t):
            name, value = m.group(1), m.group(2) if m.group(2) is not None else m.group(3)
            if name in ("hx-vals", "hx-confirm") and re.match(r"\s*(js|javascript):", value, re.I):
                errors.append(f"{path}:{line}: {name} must not use js:/javascript:")
            if name == "hx-trigger" and "[" in value:
                errors.append(f"{path}:{line}: hx-trigger filters ([...]) are forbidden")
        needs_stamp = attrs - {"nonce", "ignore"}
        if needs_stamp and not STAMP.search(t):
            errors.append(f"{path}:{line}: element with hx-* attributes lacks {{% call m::hx(...) %}}")
        if "nonce" in attrs and not STAMP.search(t):
            errors.append(f"{path}:{line}: write hx-nonce with the macro, not by hand")
    return errors


def check_script(path: Path, text: str) -> list[str]:
    errors = []
    for pattern, why in (
        (r"hx-on", "hx-on is forbidden"),
        (r"\bsafeEval\b", "safeEval is forbidden"),
        (r"config\.prefix\s*=", "setting htmx.config.prefix is forbidden"),
        (r"(?<![\w.])eval\s*\(", "eval() is forbidden"),
        (r"new\s+Function\s*\(", "new Function() is forbidden"),
    ):
        for m in re.finditer(pattern, text):
            line = text.count("\n", 0, m.start()) + 1
            errors.append(f"{path}:{line}: {why}")
    return errors


def main(root: str) -> int:
    base = Path(root)
    errors = []
    tdir = base / "crates/http/templates"
    if tdir.is_dir():
        for p in sorted(tdir.rglob("*")):
            if p.is_file():
                errors += check_template(p.relative_to(base), p.read_text(encoding="utf-8"))
    sdir = base / "web/src"
    if sdir.is_dir():
        for p in sorted(sdir.rglob("*.ts")):
            errors += check_script(p.relative_to(base), p.read_text(encoding="utf-8"))
    for e in errors:
        print(f"guard htmx: {e}", file=sys.stderr)
    return 1 if errors else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1] if len(sys.argv) > 1 else "."))
