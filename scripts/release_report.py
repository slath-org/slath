#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Writes the qualification report of a release build.

The report names the version, the commit, every artifact with its SHA-256,
the image digests, and the totals of each qualification job's JUnit file.
With --cases, it also runs the conformance report and its gate for the
release's milestone (0.N -> MN). Exits 1 if any test failed, a JUnit file
is missing, or the gate isn't met.

Usage:
  release_report.py --version X.Y.Z --commit SHA --out DIR
                    --artifact FILE... --image NAME@sha256:...
                    --junit JOB=FILE... [--cases FILE]
"""
from __future__ import annotations

import argparse
import hashlib
import json
import re
import subprocess
import sys
import xml.etree.ElementTree as ET
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent


def sha256(path: Path) -> str:
    h = hashlib.sha256()
    with path.open("rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def junit_totals(path: Path) -> dict:
    """Counts passed, failed, skipped (N/A included) tests in a JUnit file."""
    totals = {"passed": 0, "failed": 0, "skipped": 0, "failures": []}
    for tc in ET.parse(path).getroot().iter("testcase"):
        tags = {c.tag for c in tc}
        if tags & {"failure", "error"}:
            totals["failed"] += 1
            totals["failures"].append(tc.get("name", ""))
        elif "skipped" in tags:
            totals["skipped"] += 1
        else:
            totals["passed"] += 1
    return totals


def milestone(version: str) -> str | None:
    m = re.fullmatch(r"0\.(\d+)\.\d+(?:-.*)?", version)
    return f"M{m.group(1)}" if m else None


def build(args: argparse.Namespace) -> tuple[dict, list[str]]:
    problems: list[str] = []
    artifacts = []
    for a in args.artifact:
        p = Path(a)
        if not p.is_file():
            problems.append(f"artifact {a} is missing")
            continue
        artifacts.append({"name": p.name, "sha256": sha256(p), "bytes": p.stat().st_size})
    jobs = {}
    for spec in args.junit:
        job, _, file = spec.partition("=")
        p = Path(file)
        if not p.is_file():
            problems.append(f"job {job}: JUnit file {file} is missing")
            continue
        t = junit_totals(p)
        jobs[job] = t
        if t["failed"]:
            problems.append(f"job {job}: {t['failed']} test(s) failed")
        if t["passed"] + t["skipped"] == 0:
            problems.append(f"job {job}: no tests ran")
    report = {
        "version": args.version,
        "commit": args.commit,
        "artifacts": artifacts,
        "images": args.image,
        "jobs": jobs,
    }
    if args.cases:
        gate = milestone(args.version)
        cmd = [sys.executable, str(ROOT / "scripts/conformance_report.py"), "--cases", args.cases,
               "--out", str(Path(args.out) / "conformance")]
        for spec in args.junit:
            cmd += ["--junit", spec.partition("=")[2]]
        if gate:
            cmd += ["--gate", gate]
        r = subprocess.run(cmd, capture_output=True, text=True, check=False)
        report["conformance"] = {"gate": gate, "met": r.returncode == 0}
        if r.returncode != 0:
            problems.append(f"conformance gate {gate} not met: {r.stderr.strip()}")
    report["ok"] = not problems
    report["problems"] = problems
    return report, problems


def markdown(report: dict) -> str:
    lines = [f"# Slath {report['version']}: qualification report", "",
             f"- Commit: `{report['commit']}`",
             f"- Result: **{'qualified' if report['ok'] else 'NOT qualified'}**", "",
             "## Artifacts", "", "| File | SHA-256 | Bytes |", "|---|---|---|"]
    lines += [f"| `{a['name']}` | `{a['sha256']}` | {a['bytes']} |" for a in report["artifacts"]]
    lines += ["", "## Images", ""] + [f"- `{i}`" for i in report["images"]]
    lines += ["", "## Qualification jobs", "", "| Job | Passed | Failed | Skipped or N/A |", "|---|---|---|---|"]
    lines += [f"| {j} | {t['passed']} | {t['failed']} | {t['skipped']} |" for j, t in report["jobs"].items()]
    if "conformance" in report:
        c = report["conformance"]
        lines += ["", f"Conformance gate {c['gate']}: **{'met' if c['met'] else 'NOT met'}** (conformance/report.md)."]
    else:
        lines += ["", "The conformance gate is checked by the maintainer with `just conformance-report` "
                  "on these JUnit files (the cases file isn't part of the repository)."]
    if report["problems"]:
        lines += ["", "## Problems", ""] + [f"- {p}" for p in report["problems"]]
    return "\n".join(lines) + "\n"


def main(argv: list[str]) -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--version", required=True)
    ap.add_argument("--commit", required=True)
    ap.add_argument("--out", required=True)
    ap.add_argument("--artifact", action="append", default=[])
    ap.add_argument("--image", action="append", default=[])
    ap.add_argument("--junit", action="append", default=[])
    ap.add_argument("--cases")
    args = ap.parse_args(argv)
    report, problems = build(args)
    out = Path(args.out)
    out.mkdir(parents=True, exist_ok=True)
    (out / "report.json").write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
    (out / "report.md").write_text(markdown(report), encoding="utf-8")
    for p in problems:
        print(f"release-report: {p}", file=sys.stderr)
    return 1 if problems else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
