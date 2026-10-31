# SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Tests for check_license_deps.py using temporary Cargo workspaces."""
from __future__ import annotations

import subprocess
import sys
import tempfile
import textwrap
import unittest
from pathlib import Path

SCRIPT = Path(__file__).resolve().parent / "check_license_deps.py"


def make_crate(root: Path, name: str, license: str, deps: dict[str, str]) -> None:
    d = root / name
    (d / "src").mkdir(parents=True)
    dep_lines = "\n".join(
        f'{n} = {{ path = "../{n}" }}' if kind == "normal" else ""
        for n, kind in deps.items()
    )
    dev_lines = "\n".join(
        f'{n} = {{ path = "../{n}" }}' for n, kind in deps.items() if kind == "dev"
    )
    (d / "Cargo.toml").write_text(textwrap.dedent(f"""\
        [package]
        name = "{name}"
        version = "0.0.0"
        edition = "2021"
        license = "{license}"

        [dependencies]
        {dep_lines}

        [dev-dependencies]
        {dev_lines}
        """))
    (d / "src" / "lib.rs").write_text("")


def run_check(crates: list[tuple[str, str, dict[str, str]]]) -> subprocess.CompletedProcess:
    with tempfile.TemporaryDirectory() as tmp:
        root = Path(tmp)
        members = ", ".join(f'"{c[0]}"' for c in crates)
        (root / "Cargo.toml").write_text(f'[workspace]\nresolver = "2"\nmembers = [{members}]\n')
        for name, lic, deps in crates:
            make_crate(root, name, lic, deps)
        return subprocess.run(
            [sys.executable, str(SCRIPT), "--manifest-path", str(root / "Cargo.toml")],
            capture_output=True, text=True,
        )


class LicenseDepsTest(unittest.TestCase):
    def test_apache_depending_on_agpl_fails(self) -> None:
        r = run_check([
            ("server", "AGPL-3.0-or-later", {}),
            ("client", "Apache-2.0", {"server": "normal"}),
        ])
        self.assertEqual(r.returncode, 1, r.stdout + r.stderr)
        self.assertIn("client (Apache-2.0) depends on server", r.stderr)

    def test_transitive_violation_fails(self) -> None:
        r = run_check([
            ("server", "AGPL-3.0-or-later", {}),
            ("middle", "Apache-2.0", {"server": "normal"}),
            ("client", "Apache-2.0", {"middle": "normal"}),
        ])
        self.assertEqual(r.returncode, 1)
        self.assertIn("client -> middle -> server", r.stderr)

    def test_agpl_depending_on_apache_passes(self) -> None:
        r = run_check([
            ("proto", "Apache-2.0", {}),
            ("server", "AGPL-3.0-or-later", {"proto": "normal"}),
        ])
        self.assertEqual(r.returncode, 0, r.stderr)

    def test_dev_dependency_is_exempt(self) -> None:
        r = run_check([
            ("server", "AGPL-3.0-or-later", {}),
            ("client", "Apache-2.0", {"server": "dev"}),
        ])
        self.assertEqual(r.returncode, 0, r.stderr)


if __name__ == "__main__":
    unittest.main()
