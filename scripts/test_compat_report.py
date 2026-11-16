# SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
# SPDX-License-Identifier: AGPL-3.0-or-later
"""The nightly Git/jj matrix report and the shape of its workflow."""
import argparse
import sys
import tempfile
import unittest
from pathlib import Path

import yaml

sys.path.insert(0, str(Path(__file__).resolve().parent))
import compat_report  # noqa: E402

ROOT = Path(__file__).resolve().parent.parent
WORKFLOW = ROOT / ".github/workflows/compat-matrix.yml"

JUNIT = """<?xml version="1.0"?>
<testsuites><testsuite name="compat">
<testcase name="jj_prediction::conf_jj_09_predicted_ids_match_every_jj_release"/>
<testcase name="flow_parity::conf_jj_11_every_flow"><failure message="differ"/></testcase>
<testcase name="skipped"><skipped/></testcase>
</testsuite></testsuites>
"""

VERSIONS = """
[[git]]
version = "2.56.0"
sha256 = "x"
[[jj]]
version = "0.46.0"
sha256.x86_64 = "x"
"""


class Report(unittest.TestCase):
    def setUp(self):
        self.dir = Path(tempfile.mkdtemp())
        (self.dir / "versions.toml").write_text(VERSIONS)
        (self.dir / "junit.xml").write_text(JUNIT)

    def args(self, junit="junit.xml"):
        return argparse.Namespace(
            junit=str(self.dir / junit),
            git="/bin/git",
            jj=["/jj-0.46.0/bin/jj", "/jj-0.47.1/bin/jj"],
            versions=str(self.dir / "versions.toml"),
        )

    def version_of(self, binary):
        return {"/bin/git": "2.56.0"}.get(binary) or binary.split("jj-")[1].split("/")[0]

    def test_names_clients_and_the_unpinned_latest_release(self):
        report, _ = compat_report.build(self.args(), self.version_of)
        self.assertEqual(
            report["clients"],
            [
                {"tool": "git", "version": "2.56.0", "pinned": True},
                {"tool": "jj", "version": "0.46.0", "pinned": True},
                {"tool": "jj", "version": "0.47.1", "pinned": False},
            ],
        )
        self.assertEqual(report["unpinned"], ["jj 0.47.1"])
        text = compat_report.markdown(report, [])
        self.assertIn("| jj | 0.47.1 | latest release, not pinned yet |", text)

    def test_failures_and_a_missing_junit_file_are_not_green(self):
        report, problems = compat_report.build(self.args(), self.version_of)
        self.assertEqual(problems, ["1 tests failed"])
        self.assertEqual(report["tests"]["failures"], ["flow_parity::conf_jj_11_every_flow"])
        self.assertIn("`flow_parity::conf_jj_11_every_flow`", compat_report.markdown(report, problems))
        _, problems = compat_report.build(self.args("missing.xml"), self.version_of)
        self.assertTrue(problems[0].startswith("no JUnit file"))


class Workflow(unittest.TestCase):
    def setUp(self):
        self.wf = yaml.safe_load(WORKFLOW.read_text(encoding="utf-8"))
        self.job = self.wf["jobs"]["compat-matrix"]
        self.run = "\n".join(s.get("run", "") for s in self.job["steps"])

    def test_runs_nightly(self):
        on = self.wf[True] if True in self.wf else self.wf["on"]
        self.assertIn("schedule", on)
        self.assertIn("workflow_dispatch", on)

    def test_pinned_clients_and_the_latest_jj_release(self):
        self.assertIn("scripts/install-git.sh", self.run)
        self.assertIn("just tools", self.run)
        self.assertIn("scripts/install-jj-latest.sh", self.run)
        self.assertIn("SLATH_TEST_JJ", self.run)
        self.assertIn("--test compat", self.run)
        self.assertIn("scripts/compat_report.py", self.run)

    def test_publishes_the_report_and_tracks_failures_in_an_issue(self):
        steps = self.job["steps"]
        upload = [s for s in steps if s.get("uses", "").startswith("actions/upload-artifact@")]
        self.assertEqual(len(upload), 1)
        self.assertEqual(upload[0].get("if"), "always()")
        issue = [s for s in steps if "gh issue" in s.get("run", "")]
        self.assertTrue(issue, "no step opens or annotates the tracking issue")
        self.assertIn("failure()", issue[0].get("if", ""))
        self.assertEqual(self.job["permissions"].get("issues"), "write")

    def test_actions_are_pinned_to_commits(self):
        for s in self.job["steps"]:
            if "uses" in s:
                self.assertRegex(s["uses"], r"@[0-9a-f]{40}\b", s["uses"])


class ReleaseNeedsTheMatrix(unittest.TestCase):
    def test_a_red_matrix_blocks_the_release(self):
        release = (ROOT / ".github/workflows/release.yml").read_text(encoding="utf-8")
        self.assertIn("compat-matrix.yml", release)


if __name__ == "__main__":
    unittest.main()
