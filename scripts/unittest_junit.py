# SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Run the Python helper scripts' unit tests (scripts/test_*.py) and write
their results as JUnit XML, so that the conformance report counts the
`test_conf_<area>_<nn>_…` methods for their cases.

Each test is named `<module>::<method>` without the `test_` prefix, e.g.
`test_release_workflow::conf_ops_09_publish_needs_a_gate…`.

Usage: python3 scripts/unittest_junit.py OUT.xml
Exits 1 when a test fails.
"""
import sys
import time
import unittest
import xml.etree.ElementTree as ET
from pathlib import Path

SCRIPTS = Path(__file__).resolve().parent


class Result(unittest.TextTestResult):
    def __init__(self, *args, **kwargs):
        super().__init__(*args, **kwargs)
        self.cases: list[tuple[unittest.TestCase, float, str | None, str]] = []
        self._start = 0.0

    def startTest(self, test):
        self._start = time.monotonic()
        super().startTest(test)

    def _record(self, test, outcome, detail=""):
        self.cases.append((test, time.monotonic() - self._start, outcome, detail))

    def addSuccess(self, test):
        super().addSuccess(test)
        self._record(test, None)

    def addFailure(self, test, err):
        super().addFailure(test, err)
        self._record(test, "failure", self._exc_info_to_string(err, test))

    def addError(self, test, err):
        super().addError(test, err)
        self._record(test, "error", self._exc_info_to_string(err, test))

    def addSkip(self, test, reason):
        super().addSkip(test, reason)
        self._record(test, "skipped", reason)


def name(test: unittest.TestCase) -> str:
    module = type(test).__module__.rpartition(".")[2]
    return f"{module}::{test._testMethodName.removeprefix('test_')}"


def junit(cases) -> ET.Element:
    suites = ET.Element("testsuites", name="scripts")
    suite = ET.SubElement(suites, "testsuite", name="scripts", tests=str(len(cases)))
    for test, seconds, outcome, detail in cases:
        tc = ET.SubElement(suite, "testcase", classname="scripts", name=name(test), time=f"{seconds:.3f}")
        if outcome == "skipped":
            ET.SubElement(tc, "skipped", message=detail)
        elif outcome:
            ET.SubElement(tc, outcome, message=detail.splitlines()[-1] if detail else outcome).text = detail
    return suites


def main(argv: list[str]) -> int:
    if len(argv) != 1:
        print(__doc__, file=sys.stderr)
        return 2
    sys.path.insert(0, str(SCRIPTS))
    suite = unittest.defaultTestLoader.discover(str(SCRIPTS), pattern="test_*.py", top_level_dir=str(SCRIPTS))
    result = unittest.TextTestRunner(resultclass=Result, verbosity=1).run(suite)
    out = Path(argv[0])
    out.parent.mkdir(parents=True, exist_ok=True)
    ET.ElementTree(junit(result.cases)).write(out, encoding="utf-8", xml_declaration=True)
    return 0 if result.wasSuccessful() else 1


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
