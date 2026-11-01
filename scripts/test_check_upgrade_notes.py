# SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Tests for check_upgrade_notes.py."""
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent))
from check_upgrade_notes import check  # noqa: E402

GOOD = 'version = "{v}"\nschema_version = 9\nmin_upgrade_from = "0.1.0"\n'


class UpgradeNotes(unittest.TestCase):
    def tree(self, files):
        d = Path(tempfile.mkdtemp())
        (d / "upgrade-notes").mkdir()
        (d / "crates/db/src").mkdir(parents=True)
        (d / "crates/db/src/lib.rs").write_text("pub const REQUIRED_SCHEMA: i32 = 9;\n")
        for name, text in files.items():
            (d / "upgrade-notes" / name).write_text(text)
        return d

    def test_every_tag_needs_notes(self):
        d = self.tree({"0.1.0.toml": GOOD.format(v="0.1.0")})
        self.assertEqual(check(d, release_tags=["v0.1.0"]), [])
        errors = check(d, release_tags=["v0.1.0", "v0.1.1"])
        self.assertEqual(errors, ["release v0.1.1 has no upgrade-notes/0.1.1.toml"])

    def test_release_tag_mode(self):
        d = self.tree({"0.2.0.toml": GOOD.format(v="0.2.0")})
        self.assertEqual(check(d, tag="v0.2.0", release_tags=[]), [])
        self.assertTrue(check(d, tag="v0.3.0", release_tags=[]))
        d = self.tree({"0.2.0.toml": GOOD.format(v="0.2.0").replace("= 9", "= 8")})
        self.assertTrue(any("REQUIRED_SCHEMA" in e for e in check(d, tag="v0.2.0", release_tags=[])))

    def test_bad_files(self):
        d = self.tree({
            "0.1.0.toml": 'version = "0.1.1"\nschema_version = 1\nmin_upgrade_from = "0.1.0"\n',
            "0.1.1.toml": 'version = "0.1.1"\n',
            "0.1.2.toml": GOOD.format(v="0.1.2") + '[[action]]\nid = "x"\nseverity = "maybe"\ntext = "t"\n',
            "0.1.3.toml": "not toml [",
        })
        self.assertEqual(len(check(d, release_tags=[])), 4)


if __name__ == "__main__":
    unittest.main()
