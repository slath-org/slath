# SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Tests for check_asset_licenses.py."""
import json
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent))
from check_asset_licenses import check  # noqa: E402

NOTICES = """| Component | Version | License |
|---|---|---|
| htmx | 4.0.0 | 0BSD ([text](LICENSES/0BSD.txt)) | `A.woff2` |
"""


class AssetLicenses(unittest.TestCase):
    def tree(self, notices=NOTICES, fonts=("A.woff2",), dist=()):
        d = Path(tempfile.mkdtemp())
        (d / "web/src/fonts").mkdir(parents=True)
        (d / "LICENSES").mkdir()
        (d / "LICENSES/0BSD.txt").write_text("x")
        for f in fonts:
            (d / "web/src/fonts" / f).write_bytes(b"")
        (d / "web/package.json").write_text(json.dumps({"dependencies": {"htmx.org": "4.0.0"}}))
        (d / "THIRD-PARTY-NOTICES.md").write_text(notices)
        if dist:
            (d / "web/dist").mkdir()
            for f in dist:
                (d / "web/dist" / f).write_bytes(b"")
        return d

    def test_complete_tree_passes(self):
        self.assertEqual(check(self.tree(dist=("app.js", "A.woff2"))), [])

    def test_unlisted_font_fails(self):
        self.assertTrue(check(self.tree(fonts=("A.woff2", "B.woff2"))))

    def test_version_mismatch_fails(self):
        self.assertTrue(check(self.tree(notices=NOTICES.replace("4.0.0", "3.0.0"))))

    def test_unknown_dist_file_fails(self):
        self.assertTrue(check(self.tree(dist=("app.js", "font.ttf"))))

    def test_missing_license_text_fails(self):
        self.assertTrue(check(self.tree(notices=NOTICES.replace("0BSD.txt", "MIT.txt"))))

    def test_repository_passes(self):
        self.assertEqual(check(Path(__file__).resolve().parent.parent), [])


if __name__ == "__main__":
    unittest.main()
