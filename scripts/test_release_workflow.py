# SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Structure of .github/workflows/release.yml (release qualification):
the artifacts are built once, every qualification job uses them, and
nothing is published unless every required job succeeded."""
import re
import unittest
from pathlib import Path

import yaml

WORKFLOW = Path(__file__).resolve().parent.parent / ".github/workflows/release.yml"
QUALIFY = ["tests", "e2e", "compose", "setup", "upgrade-matrix"]


def load():
    return yaml.safe_load(WORKFLOW.read_text(encoding="utf-8"))["jobs"]


def needs(job):
    n = job.get("needs", [])
    return [n] if isinstance(n, str) else n


def scripts(job):
    return "\n".join(s.get("run", "") for s in job.get("steps", []))


class ReleaseWorkflow(unittest.TestCase):
    def test_conf_ops_09_publish_needs_a_gate_that_fails_on_any_non_success(self):
        jobs = load()
        self.assertEqual(needs(jobs["publish"]), ["gate"])
        gate = jobs["gate"]
        self.assertEqual(gate.get("if"), "always()")
        self.assertEqual(set(needs(gate)), {"preconditions", "build", *QUALIFY})
        # Skipped and cancelled count as failures, not only "failure".
        self.assertIn('all(.[]; .result == "success")', scripts(gate))
        # Every job except the final two is required by the gate.
        self.assertEqual(set(jobs) - {"gate", "publish"}, set(needs(gate)))

    def test_conf_ops_09_qualification_runs_on_the_built_artifacts_only(self):
        jobs = load()
        for name in QUALIFY:
            job = jobs[name]
            self.assertEqual(needs(job), ["build"], name)
            uses = [s.get("uses", "") for s in job["steps"]]
            self.assertTrue(any(u.startswith("actions/download-artifact@") for u in uses), name)
            run = scripts(job)
            self.assertNotRegex(run, r"cargo build[^\n]*-p slath(\s|$)", name)
            self.assertNotIn("docker build", run, name)
            self.assertTrue("SLATH_BIN=" in run or "SLATH_IMAGE=" in run, name)

    def test_conf_ops_09_images_are_pushed_by_digest_and_tagged_after_the_gate(self):
        jobs = load()
        build = scripts(jobs["build"])
        self.assertIn("push-by-digest=true", build)
        self.assertNotRegex(build, r"-t \"?\$IMAGE:")
        publish = scripts(jobs["publish"])
        self.assertIn("imagetools create", publish)
        self.assertIn("image-x86_64.digest", publish)
        self.assertIn("image-aarch64.digest", publish)
        # Published files are checked against the build job's hashes.
        self.assertRegex(publish, r"sha256sum -c \.\./artifacts/build-x86_64/slath-x86_64\.sha256")

    def test_red_nightlies_block_the_release(self):
        pre = scripts(load()["preconditions"])
        for wf in ["upgrade-matrix.yml", "oidc-providers.yml"]:
            self.assertIn(wf, pre)
        self.assertIn("--event schedule", pre)

    def test_actions_are_pinned_to_commits(self):
        for name, job in load().items():
            for s in job.get("steps", []):
                if "uses" in s:
                    self.assertRegex(s["uses"], r"@[0-9a-f]{40}$", f"{name}: {s['uses']}")


if __name__ == "__main__":
    unittest.main()
