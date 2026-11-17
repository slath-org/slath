# SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Publishing documentation versions to the website's docs directory."""
import json
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import publish_docs  # noqa: E402


class Publish(unittest.TestCase):
    def setUp(self):
        self.root = Path(tempfile.mkdtemp())
        self.docs = self.root / "docs"
        self.docs.mkdir()

    def site(self, version):
        s = self.root / f"site-{version}"
        (s / "user").mkdir(parents=True, exist_ok=True)
        (s / "index.html").write_text(f"<h1>{version}</h1>")
        (s / "user" / "forks.html").write_text("forks")
        return s

    def test_versions_newest_first_and_latest_is_the_newest_release(self):
        for v in ["0.3.0", "0.4.0-rc.1", "0.3.1", "0.10.0-rc.1"]:
            publish_docs.publish(self.docs, self.site(v), v)
        data = json.loads((self.docs / "versions.json").read_text())
        self.assertEqual(data["versions"], ["0.10.0-rc.1", "0.4.0-rc.1", "0.3.1", "0.3.0"])
        self.assertEqual((self.docs / "latest" / "index.html").read_text(), "<h1>0.3.1</h1>")
        self.assertTrue((self.docs / "0.3.0" / "user" / "forks.html").is_file())
        listing = (self.docs / "versions.html").read_text()
        self.assertIn('<a href="0.3.1/index.html">Slath 0.3.1</a>', listing)
        self.assertIn('href="latest/index.html"', (self.docs / "index.html").read_text())

    def test_republishing_a_version_replaces_it(self):
        publish_docs.publish(self.docs, self.site("0.3.0"), "0.3.0")
        (self.docs / "0.3.0" / "stale.html").write_text("old")
        publish_docs.publish(self.docs, self.site("0.3.0"), "0.3.0")
        self.assertFalse((self.docs / "0.3.0" / "stale.html").exists())

    def test_bad_input_is_refused(self):
        with self.assertRaises(ValueError):
            publish_docs.publish(self.docs, self.site("0.3.0"), "../../etc")
        with self.assertRaises(ValueError):
            publish_docs.publish(self.docs, self.root / "empty", "0.3.0")


if __name__ == "__main__":
    unittest.main()
