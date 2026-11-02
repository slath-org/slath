# SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Tests of check_bundle_size.py."""

import os
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent))
import check_bundle_size as c  # noqa: E402


class BundleSizeTest(unittest.TestCase):
    def setUp(self):
        self.dir = tempfile.TemporaryDirectory()
        self.dist = Path(self.dir.name)
        (self.dist / "app.js").write_text("export const x=1;")
        (self.dist / "shell.js").write_text('import{x}from"./app.js";export const s=1;import("./wysiwyg.js")')
        (self.dist / "w3c-keyname.js").write_text("export const k=1;")
        (self.dist / "source.js").write_text('import{s}from"./shell.js";')

    def tearDown(self):
        self.dir.cleanup()

    def test_small_bundles_pass(self):
        (self.dist / "wysiwyg.js").write_text('import{k}from"./w3c-keyname.js";')
        self.assertEqual(c.check(self.dist), [])

    def test_over_budget_fails(self):
        (self.dist / "wysiwyg.js").write_bytes(os.urandom(160 * 1024))
        errors = c.check(self.dist)
        self.assertTrue(any("wysiwyg.js mode" in e for e in errors), errors)

    def test_editor_code_in_app_fails(self):
        (self.dist / "wysiwyg.js").write_text("")
        (self.dist / "app.js").write_text("class ProseMirror{}")
        self.assertTrue(any("app.js contains" in e for e in c.check(self.dist)))


if __name__ == "__main__":
    unittest.main()
