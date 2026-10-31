# SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Tests for conformance_report.py. Run: python3 -m unittest scripts/test_conformance_report.py"""
from __future__ import annotations

import json
import subprocess
import sys
import tempfile
import textwrap
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import conformance_report as cr  # noqa: E402

SCRIPT = Path(__file__).resolve().parent / "conformance_report.py"

CASES = textwrap.dedent("""\
    | ID | Gate | Criticality | Scenario |
    | --- | --- | --- | --- |
    | **FOO-01** | M0 | MUST-v1 | First. |
    | **FOO-02** | M0 | MUST-v1 | Second. |
    | **BAR-01** | M0 | MUST-v1 | Third. |
    | **FOO-03** | M1 | MUST-v1 | Later. |
    | **FOO-08** | — | FUTURE | Never. |
    | **A1B-01** | M1 | MUST-v1 | Other area. |
    """)


def junit(cases: list[tuple[str, str]]) -> str:
    body = []
    for name, outcome in cases:
        inner = {"pass": "", "fail": "<failure message='x'/>", "skip": "<skipped/>",
                 "flaky": "<flakyFailure message='x'/>", "error": "<error/>"}[outcome]
        body.append(f'<testcase name="{name}" classname="slath-tests::compat">{inner}</testcase>')
    return f'<testsuites><testsuite name="s">{"".join(body)}</testsuite></testsuites>'


class ParseTest(unittest.TestCase):
    def test_parse_cases(self) -> None:
        cases = cr.parse_cases(CASES)
        self.assertEqual(list(cases), ["FOO-01", "FOO-02", "BAR-01", "FOO-03", "FOO-08", "A1B-01"])
        self.assertEqual(cases["FOO-08"].gate, None)
        self.assertEqual(cases["FOO-08"].criticality, "FUTURE")
        self.assertEqual(cases["FOO-03"].gate, "M1")

    def test_duplicate_case_is_an_error(self) -> None:
        with self.assertRaises(ValueError):
            cr.parse_cases(CASES + "| **FOO-01** | M0 | MUST-v1 | again |\n")

    def test_case_id_mapping(self) -> None:
        self.assertEqual(cr.case_id_for("conf_foo_01_https_clone"), "FOO-01")
        self.assertEqual(cr.case_id_for("compat::suite::conf_foo_01_v2_fetch"), "FOO-01")
        self.assertEqual(cr.case_id_for("conf_a1b_01_error_page"), "A1B-01")
        self.assertIsNone(cr.case_id_for("not_conformance"))
        self.assertIsNone(cr.case_id_for("conf_git_1_bad_number"))
        self.assertIsNone(cr.case_id_for("myconf_foo_01_x"))

    def test_gating(self) -> None:
        cases = cr.parse_cases(CASES)
        self.assertTrue(cases["FOO-01"].gated_by("M0"))
        self.assertFalse(cases["FOO-03"].gated_by("M0"))
        self.assertTrue(cases["FOO-03"].gated_by("M1"))
        self.assertFalse(cases["FOO-08"].gated_by("M7"))


class RunTest(unittest.TestCase):
    def run_script(self, results: list[tuple[str, str]], *extra: str) -> tuple[int, dict, str]:
        with tempfile.TemporaryDirectory() as tmp:
            d = Path(tmp)
            (d / "cases.md").write_text(CASES)
            (d / "junit.xml").write_text(junit(results))
            p = subprocess.run(
                [sys.executable, str(SCRIPT), "--cases", str(d / "cases.md"),
                 "--junit", str(d / "junit.xml"), "--out", str(d / "out"), *extra],
                capture_output=True, text=True,
            )
            report = json.loads((d / "out" / "report.json").read_text())
            md = (d / "out" / "report.md").read_text()
            return p.returncode, report, md

    @staticmethod
    def statuses(report: dict) -> dict[str, str]:
        return {c["test_id"]: c["status"] for c in report["cases"]}

    def test_statuses(self) -> None:
        _, report, md = self.run_script([
            ("conf_foo_01_a", "pass"), ("conf_foo_01_b", "flaky"),
            ("conf_foo_02_a", "pass"), ("conf_foo_02_b", "fail"),
            ("conf_bar_01_a", "skip"),
            ("unrelated_test", "fail"),
        ])
        s = self.statuses(report)
        self.assertEqual(s["FOO-01"], "PASS")
        self.assertEqual(s["FOO-02"], "FAIL")
        self.assertEqual(s["BAR-01"], "SKIP")
        self.assertEqual(s["FOO-03"], "UNKNOWN")
        self.assertIn("| FOO-01 | M0 | MUST-v1 | PASS |", md)

    def test_error_counts_as_failure(self) -> None:
        _, report, _ = self.run_script([("conf_foo_01_a", "error")])
        self.assertEqual(self.statuses(report)["FOO-01"], "FAIL")

    def test_gate_fails_unless_all_gated_cases_pass(self) -> None:
        code, report, _ = self.run_script([("conf_foo_01_a", "pass")], "--gate", "M0")
        self.assertEqual(code, 1)
        self.assertFalse(report["gate"]["met"])
        self.assertIn("FOO-02 is UNKNOWN", report["gate"]["failures"])

    def test_gate_passes_when_gated_cases_pass(self) -> None:
        code, report, md = self.run_script(
            [("conf_foo_01_a", "pass"), ("conf_foo_02_a", "pass"), ("conf_bar_01_a", "pass")],
            "--gate", "M0",
        )
        self.assertEqual(code, 0)
        self.assertTrue(report["gate"]["met"])
        self.assertIn("Gate M0: **met**", md)

    def test_environment_is_recorded(self) -> None:
        _, report, _ = self.run_script([])
        env = report["environment"]
        for key in ("harness_commit", "git_version", "jj_version", "rustc_version"):
            self.assertIn(key, env)
        self.assertTrue(env["git_version"].startswith("git version"))


if __name__ == "__main__":
    unittest.main()
