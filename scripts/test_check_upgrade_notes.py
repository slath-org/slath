# SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Tests for check_upgrade_notes.py."""
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent))
from check_upgrade_notes import check  # noqa: E402

GOOD = 'version = "{v}"\nschema_version = 109\nmin_upgrade_from = "0.3.0"\n'


class UpgradeNotes(unittest.TestCase):
    def tree(self, files):
        d = Path(tempfile.mkdtemp())
        (d / "upgrade-notes").mkdir()
        (d / "crates/db/src").mkdir(parents=True)
        (d / "crates/db/src/lib.rs").write_text("pub const REQUIRED_SCHEMA: i32 = 109;\n")
        for name, text in files.items():
            (d / "upgrade-notes" / name).write_text(text)
        return d

    def test_every_tag_needs_notes(self):
        d = self.tree({"0.3.0.toml": GOOD.format(v="0.3.0")})
        self.assertEqual(check(d, release_tags=["v0.3.0"]), [])
        errors = check(d, release_tags=["v0.3.0", "v0.3.1"])
        self.assertEqual(errors, ["release v0.3.1 has no upgrade-notes/0.3.1.toml"])

    def test_previews_have_no_notes(self):
        d = self.tree({"0.3.0.toml": GOOD.format(v="0.3.0")})
        self.assertEqual(check(d, release_tags=["v0.1.0", "v0.1.1", "v0.2.0", "v0.3.0"]), [])
        d = self.tree({"0.2.0.toml": GOOD.format(v="0.2.0")})
        self.assertEqual(
            check(d, release_tags=[]),
            ["0.2.0.toml: releases before 0.3.0 were previews and have no upgrade notes"],
        )

    def test_release_tag_mode(self):
        d = self.tree({"0.4.0.toml": GOOD.format(v="0.4.0")})
        self.assertEqual(check(d, tag="v0.4.0", release_tags=[]), [])
        self.assertTrue(check(d, tag="v0.5.0", release_tags=[]))
        d = self.tree({"0.4.0.toml": GOOD.format(v="0.4.0").replace("= 109", "= 108")})
        self.assertTrue(any("REQUIRED_SCHEMA" in e for e in check(d, tag="v0.4.0", release_tags=[])))

    def test_bad_files(self):
        d = self.tree({
            "0.3.0.toml": 'version = "0.3.1"\nschema_version = 101\nmin_upgrade_from = "0.3.0"\n',
            "0.3.1.toml": 'version = "0.3.1"\n',
            "0.3.2.toml": GOOD.format(v="0.3.2") + '[[action]]\nid = "x"\nseverity = "maybe"\ntext = "t"\n',
            "0.3.3.toml": "not toml [",
        })
        self.assertEqual(len(check(d, release_tags=[])), 4)


if __name__ == "__main__":
    unittest.main()
