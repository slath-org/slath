#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Writes the report of the nightly Git/jj compatibility matrix.

The report names every client tested (its version, and whether it's pinned
in tests/compat/versions.toml or the latest release, not pinned yet), the
totals of the suite's JUnit file, and the failed tests. It writes
report.md and report.json to --out and exits 1 if a test failed or the
JUnit file is missing.

Usage:
  compat_report.py --out DIR --junit FILE --git BIN --jj BIN...
                   [--versions tests/compat/versions.toml]
"""
from __future__ import annotations

import argparse
import json
import re
import subprocess
import sys
import tomllib
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from release_report import junit_totals  # noqa: E402

ROOT = Path(__file__).resolve().parent.parent


def tool_version(binary: str) -> str:
    out = subprocess.run([binary, "--version"], capture_output=True, text=True, check=True)
    m = re.search(r"\d+\.\d+\.\d+", out.stdout)
    return m.group(0) if m else out.stdout.strip()


def build(args: argparse.Namespace, version_of=tool_version) -> tuple[dict, list[str]]:
    pinned = tomllib.loads(Path(args.versions).read_text())
    problems: list[str] = []
    clients = []
    for tool, binaries in (("git", [args.git]), ("jj", args.jj)):
        pins = {e["version"] for e in pinned.get(tool, [])}
        for b in binaries:
            v = version_of(b)
            clients.append({"tool": tool, "version": v, "pinned": v in pins})
    report = {"clients": clients, "tests": None}
    junit = Path(args.junit)
    if junit.is_file():
        report["tests"] = junit_totals(junit)
        if report["tests"]["failed"]:
            problems.append(f"{report['tests']['failed']} tests failed")
    else:
        problems.append(f"no JUnit file at {junit}")
    unpinned = [c for c in clients if not c["pinned"]]
    report["unpinned"] = [f"{c['tool']} {c['version']}" for c in unpinned]
    return report, problems


def markdown(report: dict, problems: list[str]) -> str:
    lines = ["# Git/jj compatibility matrix", "", "| Client | Version | |", "|---|---|---|"]
    for c in report["clients"]:
        note = "pinned" if c["pinned"] else "latest release, not pinned yet"
        lines.append(f"| {c['tool']} | {c['version']} | {note} |")
    lines.append("")
    t = report["tests"]
    if t is not None:
        lines.append(f"Tests: {t['passed']} passed, {t['failed']} failed, {t['skipped']} skipped.")
        if t["failures"]:
            lines += ["", "Failed:", ""] + [f"- `{f}`" for f in t["failures"]]
    if problems:
        lines += ["", "**Not green:** " + "; ".join(problems) + "."]
    else:
        lines += ["", "Green."]
    return "\n".join(lines) + "\n"


def main(argv: list[str]) -> int:
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("--out", required=True)
    p.add_argument("--junit", required=True)
    p.add_argument("--git", required=True)
    p.add_argument("--jj", nargs="+", required=True)
    p.add_argument("--versions", default=str(ROOT / "tests/compat/versions.toml"))
    args = p.parse_args(argv)
    report, problems = build(args)
    out = Path(args.out)
    out.mkdir(parents=True, exist_ok=True)
    (out / "report.json").write_text(json.dumps(report, indent=2) + "\n")
    text = markdown(report, problems)
    (out / "report.md").write_text(text)
    print(text)
    return 1 if problems else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
