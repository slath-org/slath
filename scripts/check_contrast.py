#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Checks the Weave color tokens.

Reads the token sets from web/src/tokens.json and the declared pairs from
web/src/contrast-pairs.json, then verifies, in every token set:

  - each contrast pair meets its WCAG 2.2 minimum ratio (4.5 for text,
    3 for large text and UI boundaries);
  - each "distinct" pair is at least the given OKLab distance apart
    (ΔE × 100), so semantic colors don't read as the accent.

Usage: check_contrast.py [--table] [ROOT]
  --table  print a Markdown table of every pair and its result instead.
Exit status 1 if any pair fails.
"""
import json
import math
import sys
from pathlib import Path


def parse_hex(c: str) -> tuple[float, float, float]:
    c = c.strip().lstrip("#")
    if len(c) == 3:
        c = "".join(ch * 2 for ch in c)
    if len(c) != 6:
        raise ValueError(f"not a #rrggbb color: {c!r}")
    return tuple(int(c[i : i + 2], 16) / 255 for i in (0, 2, 4))  # type: ignore[return-value]


def _linear(v: float) -> float:
    return v / 12.92 if v <= 0.04045 else ((v + 0.055) / 1.055) ** 2.4


def luminance(c: str) -> float:
    r, g, b = (_linear(v) for v in parse_hex(c))
    return 0.2126 * r + 0.7152 * g + 0.0722 * b


def contrast(a: str, b: str) -> float:
    la, lb = luminance(a), luminance(b)
    hi, lo = max(la, lb), min(la, lb)
    return (hi + 0.05) / (lo + 0.05)


def oklab(c: str) -> tuple[float, float, float]:
    r, g, b = (_linear(v) for v in parse_hex(c))
    l = 0.4122214708 * r + 0.5363325363 * g + 0.0514459929 * b
    m = 0.2119034982 * r + 0.6806995451 * g + 0.1073969566 * b
    s = 0.0883024619 * r + 0.2817188376 * g + 0.6299787005 * b
    l_, m_, s_ = (math.copysign(abs(x) ** (1 / 3), x) for x in (l, m, s))
    return (
        0.2104542553 * l_ + 0.7936177850 * m_ - 0.0040720468 * s_,
        1.9779984951 * l_ - 2.4285922050 * m_ + 0.4505937099 * s_,
        0.0259040371 * l_ + 0.7827717662 * m_ - 0.8086757660 * s_,
    )


def delta_e(a: str, b: str) -> float:
    """OKLab Euclidean distance, scaled by 100."""
    return 100 * math.dist(oklab(a), oklab(b))


def resolve(tokens: dict[str, str], name: str) -> str:
    seen = set()
    v = tokens[name]
    while v.startswith("var(--"):
        ref = v[len("var(--") : -1]
        if ref in seen:
            raise ValueError(f"token cycle at {name}")
        seen.add(ref)
        v = tokens[ref]
    return v


def evaluate(tokens_doc: dict, pairs_doc: dict) -> list[dict]:
    results = []
    for set_name, tokens in tokens_doc["colors"].items():
        for p in pairs_doc["contrast"]:
            for fg in p["fg"] if isinstance(p["fg"], list) else [p["fg"]]:
                for bg in p["bg"] if isinstance(p["bg"], list) else [p["bg"]]:
                    ratio = contrast(resolve(tokens, fg), resolve(tokens, bg))
                    results.append({
                        "set": set_name, "kind": "contrast", "a": fg, "b": bg,
                        "value": ratio, "min": p["min"], "ok": ratio >= p["min"],
                    })
        for p in pairs_doc["distinct"]:
            for a in p["a"] if isinstance(p["a"], list) else [p["a"]]:
                for b in p["b"] if isinstance(p["b"], list) else [p["b"]]:
                    d = delta_e(resolve(tokens, a), resolve(tokens, b))
                    results.append({
                        "set": set_name, "kind": "distinct", "a": a, "b": b,
                        "value": d, "min": p["min_delta_e"], "ok": d >= p["min_delta_e"],
                    })
    return results


def load(root: Path) -> tuple[dict, dict]:
    tokens = json.loads((root / "web/src/tokens.json").read_text(encoding="utf-8"))
    pairs = json.loads((root / "web/src/contrast-pairs.json").read_text(encoding="utf-8"))
    return tokens, pairs


def main(argv: list[str]) -> int:
    table = "--table" in argv
    args = [a for a in argv if not a.startswith("--")]
    root = Path(args[0]) if args else Path(__file__).resolve().parent.parent
    results = evaluate(*load(root))
    if table:
        tokens_doc, _ = load(root)
        sets = list(tokens_doc["colors"])
        print("| Token | " + " | ".join(sets) + " |")
        print("|---|" + "---|" * len(sets))
        for name in tokens_doc["colors"]["light"]:
            print(f"| `--{name}` | " + " | ".join(f"`{tokens_doc['colors'][s][name]}`" for s in sets) + " |")
        print()
        print("| Pair | Required | " + " | ".join(sets) + " |")
        print("|---|---|" + "---|" * len(sets))
        rows: dict[tuple, dict] = {}
        for r in results:
            rows.setdefault((r["kind"], r["a"], r["b"], r["min"]), {})[r["set"]] = r
        for (kind, a, b, m), by_set in rows.items():
            req = f"{m}:1" if kind == "contrast" else f"ΔE {m}"
            cells = []
            for s in sets:
                r = by_set[s]
                v = f"{r['value']:.2f}" if kind == "contrast" else f"{r['value']:.1f}"
                cells.append(v + ("" if r["ok"] else " ✗"))
            sep = " on " if kind == "contrast" else " vs "
            print(f"| `{a}`{sep}`{b}` | {req} | " + " | ".join(cells) + " |")
        return 0
    failed = [r for r in results if not r["ok"]]
    for r in failed:
        what = "contrast" if r["kind"] == "contrast" else "ΔE"
        print(f"contrast: {r['set']}: {r['a']} vs {r['b']}: {what} {r['value']:.2f} < {r['min']}", file=sys.stderr)
    if failed:
        return 1
    print(f"contrast: ok ({len(results)} checks)")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
