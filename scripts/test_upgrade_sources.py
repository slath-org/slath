# SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Which releases the upgrade matrix upgrades from (upgrade_sources.py)."""
import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent))
from upgrade_sources import sources  # noqa: E402


class UpgradeSources(unittest.TestCase):
    def test_previews_and_other_tags_are_never_sources(self):
        self.assertEqual(sources(["v0.1.0", "v0.1.1", "v0.2.0", "nightly", "v0.3"]), [])

    def test_candidates_of_the_coming_release_are_sources(self):
        self.assertEqual(
            sources(["v0.2.0", "v0.3.0-rc.10", "v0.3.0-rc.2", "v0.3.0-rc.1"]),
            ["v0.3.0-rc.1", "v0.3.0-rc.2", "v0.3.0-rc.10"],
        )

    def test_the_tag_being_built_is_left_out(self):
        self.assertEqual(sources(["v0.3.0-rc.1", "v0.3.0-rc.2"], exclude="v0.3.0-rc.2"), ["v0.3.0-rc.1"])
        self.assertEqual(sources(["v0.3.0-rc.1", "v0.3.0"], exclude="v0.3.0"), ["v0.3.0-rc.1"])

    def test_candidates_replaced_by_their_release_are_dropped(self):
        self.assertEqual(
            sources(["v0.3.0-rc.1", "v0.3.0", "v0.3.1", "v0.4.0-rc.1"]),
            ["v0.3.0", "v0.3.1", "v0.4.0-rc.1"],
        )


if __name__ == "__main__":
    unittest.main()
