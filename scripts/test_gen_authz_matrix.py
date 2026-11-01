# SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
# SPDX-License-Identifier: AGPL-3.0-or-later
"""The committed authorization matrix matches the transcribed table."""
import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent))
import gen_authz_matrix  # noqa: E402


class Matrix(unittest.TestCase):
    def test_csv_is_generated_from_the_table(self):
        self.assertEqual(gen_authz_matrix.main(["--check"]), 0)

    def test_every_action_has_a_scope(self):
        self.assertEqual(set(gen_authz_matrix.SCOPE), set(gen_authz_matrix.TABLE))


if __name__ == "__main__":
    unittest.main()
