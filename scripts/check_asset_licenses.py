#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Checks that every bundled third-party asset has a license entry.

- every font file in web/src/fonts is named in THIRD-PARTY-NOTICES.md;
- every runtime dependency of web/package.json (and lucide-static, the icon
  source) is listed there with its exact version;
- every file in web/dist (when built) is either Slath's own output or named
  in the notices;
- the license texts the notices link to exist under LICENSES/.

Usage: check_asset_licenses.py [ROOT]
"""
import json
import re
import sys
from pathlib import Path

OWN_OUTPUT = {"app.js", "app.js.map", "style.css", "icons.svg"}
NOTICE_NAMES = {"htmx.org": "htmx", "lucide-static": "Lucide"}


def check(root: Path) -> list[str]:
    errors = []
    notices_path = root / "THIRD-PARTY-NOTICES.md"
    if not notices_path.is_file():
        return ["THIRD-PARTY-NOTICES.md is missing"]
    notices = notices_path.read_text(encoding="utf-8")
    for f in sorted((root / "web/src/fonts").glob("*.woff2")):
        if f"`{f.name}`" not in notices:
            errors.append(f"font {f.name} has no entry in THIRD-PARTY-NOTICES.md")
    pkg = json.loads((root / "web/package.json").read_text(encoding="utf-8"))
    deps = dict(pkg.get("dependencies", {}))
    if "lucide-static" in pkg.get("devDependencies", {}):
        deps["lucide-static"] = pkg["devDependencies"]["lucide-static"]
    for name, version in deps.items():
        label = NOTICE_NAMES.get(name, name)
        row = re.search(rf"^\| {re.escape(label)} \| ([^|]+) \|", notices, re.M)
        if not row:
            errors.append(f"dependency {name} has no row in THIRD-PARTY-NOTICES.md")
        elif row.group(1).strip() != version:
            errors.append(f"dependency {name} is {version} but THIRD-PARTY-NOTICES.md says {row.group(1).strip()}")
    dist = root / "web/dist"
    if dist.is_dir():
        for f in sorted(p for p in dist.rglob("*") if p.is_file()):
            rel = f.relative_to(dist).as_posix()
            if rel not in OWN_OUTPUT and f"`{rel}`" not in notices:
                errors.append(f"web/dist/{rel} is neither Slath's own output nor listed in THIRD-PARTY-NOTICES.md")
    for link in re.findall(r"\]\((LICENSES/[^)]+)\)", notices):
        if not (root / link).is_file():
            errors.append(f"THIRD-PARTY-NOTICES.md links to missing {link}")
    return errors


def main(argv: list[str]) -> int:
    root = Path(argv[0]) if argv else Path(__file__).resolve().parent.parent
    errors = check(root)
    for e in errors:
        print(f"asset-licenses: {e}", file=sys.stderr)
    if errors:
        return 1
    print("asset-licenses: ok")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
