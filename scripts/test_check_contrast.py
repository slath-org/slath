# SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Tests for check_contrast.py and build_tokens.py."""
import copy
import json
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent))
import build_tokens  # noqa: E402
import check_contrast as cc  # noqa: E402

ROOT = Path(__file__).resolve().parent.parent


class Formulas(unittest.TestCase):
    def test_wcag_reference_values(self):
        self.assertAlmostEqual(cc.contrast("#000000", "#FFFFFF"), 21.0, places=2)
        self.assertAlmostEqual(cc.contrast("#777777", "#FFFFFF"), 4.48, places=2)
        self.assertAlmostEqual(cc.contrast("#FFF", "#FFF"), 1.0)

    def test_oklab_distance(self):
        self.assertEqual(cc.delta_e("#123456", "#123456"), 0)
        self.assertGreater(cc.delta_e("#FF0000", "#00FF00"), 50)


class RealTokens(unittest.TestCase):
    def setUp(self):
        self.tokens, self.pairs = cc.load(ROOT)

    def test_all_pairs_pass(self):
        failed = [r for r in cc.evaluate(self.tokens, self.pairs) if not r["ok"]]
        self.assertEqual(failed, [])

    def test_insufficient_value_fails(self):
        bad = copy.deepcopy(self.tokens)
        bad["colors"]["dark"]["text-secondary"] = "#5A615C"  # too dark on dark surfaces
        failed = [r for r in cc.evaluate(bad, self.pairs) if not r["ok"]]
        self.assertTrue(any(r["a"] == "text-secondary" and r["set"] == "dark" for r in failed))

    def test_semantic_too_close_to_accent_fails(self):
        bad = copy.deepcopy(self.tokens)
        bad["colors"]["light"]["success"] = bad["colors"]["light"]["accent"]
        failed = [r for r in cc.evaluate(bad, self.pairs) if not r["ok"]]
        self.assertTrue(any(r["kind"] == "distinct" and r["a"] == "success" for r in failed))

    def test_check_mode_detects_stale_css(self):
        with tempfile.TemporaryDirectory() as d:
            (Path(d) / "web/src").mkdir(parents=True)
            (Path(d) / "web/src/tokens.json").write_text(json.dumps(self.tokens))
            self.assertEqual(build_tokens.main([d]), 0)
            self.assertEqual(build_tokens.main(["--check", d]), 0)
            (Path(d) / "web/src/tokens.css").write_text("stale")
            self.assertEqual(build_tokens.main(["--check", d]), 1)

    def test_generated_css_is_current(self):
        self.assertEqual(build_tokens.main(["--check", str(ROOT)]), 0)


if __name__ == "__main__":
    unittest.main()
