# SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Tests for release_report.py."""
import hashlib
import json
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent))
import release_report as rr  # noqa: E402

PASS = '<testsuites><testsuite><testcase name="a"/><testcase name="b"><skipped message="N/A: x"/></testcase></testsuite></testsuites>'
FAIL = '<testsuites><testsuite><testcase name="a"><failure/></testcase></testsuite></testsuites>'


class ReleaseReport(unittest.TestCase):
    def run_report(self, junit: str, artifact: bool = True):
        d = Path(tempfile.mkdtemp())
        (d / "slath-0.3.0-x86_64-linux").write_bytes(b"binary")
        (d / "tests.xml").write_text(junit)
        argv = ["--version", "0.3.0", "--commit", "abc", "--out", str(d / "out"),
                "--image", "ghcr.io/slath-org/slath@sha256:00 (x86_64)",
                "--junit", f"tests={d / 'tests.xml'}"]
        if artifact:
            argv += ["--artifact", str(d / "slath-0.3.0-x86_64-linux")]
        else:
            argv += ["--artifact", str(d / "missing")]
        code = rr.main(argv)
        report = json.loads((d / "out/report.json").read_text())
        return code, report, (d / "out/report.md").read_text()

    def test_report_names_artifacts_with_digests(self):
        code, report, md = self.run_report(PASS)
        self.assertEqual(code, 0)
        digest = hashlib.sha256(b"binary").hexdigest()
        self.assertEqual(report["artifacts"][0]["sha256"], digest)
        self.assertIn(digest, md)
        self.assertIn("sha256:00", md)
        self.assertEqual(report["jobs"]["tests"]["passed"], 1)
        self.assertEqual(report["jobs"]["tests"]["skipped"], 1)

    def test_failed_tests_or_missing_files_fail_the_report(self):
        code, report, md = self.run_report(FAIL)
        self.assertEqual(code, 1)
        self.assertIn("NOT qualified", md)
        code, report, _ = self.run_report(PASS, artifact=False)
        self.assertEqual(code, 1)
        self.assertTrue(any("missing" in p for p in report["problems"]))

    def test_milestone_of_a_release(self):
        self.assertEqual(rr.milestone("0.3.0"), "M3")
        self.assertEqual(rr.milestone("0.12.1-rc.1"), "M12")
        self.assertIsNone(rr.milestone("1.0.0"))


if __name__ == "__main__":
    unittest.main()
