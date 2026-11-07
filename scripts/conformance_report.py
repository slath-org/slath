#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Build a conformance report from case tables and JUnit test results.

The cases file is Markdown with table rows of the form
`| **<AREA>-<nn>** | <gate M<n> or —> | <criticality> | <scenario> |`.
Tests named `conf_<area>_<nn>_<description>` provide evidence for case
`<AREA>-<nn>`. A case is PASS if at least one test maps to it and all mapped
tests pass, FAIL if any mapped test fails, N/A if every mapped test was
skipped with a message starting "N/A:" (the case doesn't apply yet, with
the reason), SKIP if all mapped tests were skipped, and UNKNOWN if no test
maps to it.

Writes <out>/report.json and <out>/report.md. With `--gate M<n>`, exits 1
unless every case gated at or before M<n> (and not FUTURE) is PASS or N/A.

Usage:
  conformance_report.py --cases FILE [--junit FILE]...
                        [--out target/conformance] [--gate M<n>]
"""
from __future__ import annotations

import argparse
import datetime as dt
import json
import re
import subprocess
import sys
import xml.etree.ElementTree as ET
from dataclasses import dataclass, field
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
CASE_ROW = re.compile(
    r"^\|\s*\*\*(?P<id>[A-Z0-9]+-\d{2})\*\*\s*\|\s*(?P<gate>M\d+|—|-)\s*\|"
    r"\s*(?P<crit>[A-Z]+(?:-v1)?)\s*\|\s*(?P<text>.*?)\s*\|\s*$"
)
TEST_NAME = re.compile(r"(?:^|::)conf_(?P<area>[a-z0-9]+)_(?P<nn>\d{2})_[A-Za-z0-9_]+$")


@dataclass
class Case:
    id: str
    gate: str | None
    criticality: str
    scenario: str
    tests: list[dict] = field(default_factory=list)

    @property
    def reasons(self) -> list[str]:
        return sorted({t["reason"] for t in self.tests if t.get("reason")})

    @property
    def status(self) -> str:
        if not self.tests:
            return "UNKNOWN"
        outcomes = {t["outcome"] for t in self.tests}
        if "fail" in outcomes:
            return "FAIL"
        if outcomes == {"na"}:
            return "N/A"
        if outcomes == {"skip"}:
            return "SKIP"
        if "pass" in outcomes:
            return "PASS"
        return "UNKNOWN"

    def gated_by(self, milestone: str) -> bool:
        if self.gate is None or self.criticality == "FUTURE":
            return False
        return int(self.gate[1:]) <= int(milestone[1:])


def parse_cases(text: str) -> dict[str, Case]:
    cases: dict[str, Case] = {}
    for line in text.splitlines():
        m = CASE_ROW.match(line)
        if not m:
            continue
        gate = m["gate"] if m["gate"].startswith("M") else None
        if m["id"] in cases:
            raise ValueError(f"duplicate case {m['id']}")
        cases[m["id"]] = Case(m["id"], gate, m["crit"], m["text"])
    return cases


def case_id_for(test_name: str) -> str | None:
    m = TEST_NAME.search(test_name)
    return f"{m['area'].upper()}-{m['nn']}" if m else None


def parse_junit(path: Path) -> list[dict]:
    """Returns {name, classname, outcome} for every testcase in a JUnit file."""
    results = []
    for tc in ET.parse(path).getroot().iter("testcase"):
        tags = {child.tag for child in tc}
        skipped = tc.find("skipped")
        message = (skipped.get("message") or "") if skipped is not None else ""
        reason = None
        # nextest marks tests that passed on retry with <flakyFailure>; they pass.
        if tags & {"failure", "error"}:
            outcome = "fail"
        elif message.startswith("N/A:"):
            outcome, reason = "na", message[len("N/A:"):].strip()
        elif "skipped" in tags:
            outcome = "skip"
        else:
            outcome = "pass"
        r = {"name": tc.get("name", ""), "classname": tc.get("classname", ""), "outcome": outcome}
        if reason:
            r["reason"] = reason
        results.append(r)
    return results


def tool_version(cmd: list[str]) -> str | None:
    try:
        out = subprocess.run(cmd, capture_output=True, text=True, timeout=10, check=True)
    except (OSError, subprocess.SubprocessError):
        return None
    return out.stdout.strip().splitlines()[0] if out.stdout.strip() else None


def build_report(cases: dict[str, Case], results: list[dict], env: dict) -> tuple[dict, list[str]]:
    unmapped = []
    for r in results:
        cid = case_id_for(r["name"])
        if cid is None:
            continue
        if cid not in cases:
            unmapped.append(f"{r['name']} -> {cid} (no such case)")
            continue
        cases[cid].tests.append(r)
    report = {
        "generated_at": dt.datetime.now(dt.timezone.utc).isoformat(timespec="seconds"),
        "environment": env,
        "cases": [
            {
                "test_id": c.id,
                "gate": c.gate,
                "criticality": c.criticality,
                "status": c.status,
                "tests": sorted(t["name"] for t in c.tests),
                **({"reasons": c.reasons} if c.reasons else {}),
            }
            for c in cases.values()
        ],
    }
    return report, unmapped


def render_markdown(report: dict, gate: str | None, gate_failures: list[str]) -> str:
    env = report["environment"]
    lines = ["# Conformance report", ""]
    lines += [f"- Generated: {report['generated_at']}"]
    lines += [f"- {k}: `{v}`" for k, v in env.items()]
    if gate:
        verdict = "met" if not gate_failures else f"NOT met ({len(gate_failures)} case(s))"
        lines.append(f"- Gate {gate}: **{verdict}**")
    counts: dict[str, int] = {}
    for c in report["cases"]:
        counts[c["status"]] = counts.get(c["status"], 0) + 1
    lines += ["", " · ".join(f"{k}: {v}" for k, v in sorted(counts.items())), ""]
    lines += ["| Case | Gate | Criticality | Status | Tests |", "|---|---|---|---|---|"]
    for c in report["cases"]:
        tests = ", ".join(f"`{t}`" for t in c["tests"]) or "—"
        if c.get("reasons"):
            tests += " (" + "; ".join(c["reasons"]) + ")"
        lines.append(
            f"| {c['test_id']} | {c['gate'] or '—'} | {c['criticality']} | {c['status']} | {tests} |"
        )
    return "\n".join(lines) + "\n"


def main(argv: list[str]) -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--cases", type=Path, required=True)
    ap.add_argument("--junit", type=Path, action="append", default=[])
    ap.add_argument("--out", type=Path, default=ROOT / "target" / "conformance")
    ap.add_argument("--gate", type=lambda s: s if re.fullmatch(r"M\d+", s) else ap.error("gate must be M<n>"))
    args = ap.parse_args(argv)

    if not args.cases.is_file():
        print(f"conformance: {args.cases} not found", file=sys.stderr)
        return 2
    cases = parse_cases(args.cases.read_text(encoding="utf-8"))
    results: list[dict] = []
    for j in args.junit:
        if j.is_file():
            results += parse_junit(j)
        else:
            print(f"conformance: warning: {j} not found", file=sys.stderr)

    env = {
        "harness_commit": tool_version(["git", "-C", str(ROOT), "rev-parse", "HEAD"]),
        "git_version": tool_version(["git", "--version"]),
        "jj_version": tool_version(["jj", "--version"]),
        "rustc_version": tool_version(["rustc", "--version"]),
    }
    report, unmapped = build_report(cases, results, env)
    gate_failures = []
    if args.gate:
        gate_failures = [
            f"{c.id} is {c.status}" for c in cases.values() if c.gated_by(args.gate) and c.status not in ("PASS", "N/A")
        ]
        report["gate"] = {"milestone": args.gate, "met": not gate_failures, "failures": gate_failures}

    args.out.mkdir(parents=True, exist_ok=True)
    (args.out / "report.json").write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
    (args.out / "report.md").write_text(render_markdown(report, args.gate, gate_failures), encoding="utf-8")

    for u in unmapped:
        print(f"conformance: warning: test {u}", file=sys.stderr)
    statuses = [c["status"] for c in report["cases"]]
    print(
        f"conformance: {len(statuses)} cases, {statuses.count('PASS')} PASS, "
        f"{statuses.count('FAIL')} FAIL, {statuses.count('UNKNOWN')} UNKNOWN -> {args.out}/report.md"
    )
    if gate_failures:
        print(f"conformance: gate {args.gate} not met:", file=sys.stderr)
        for f in gate_failures:
            print(f"  {f}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
