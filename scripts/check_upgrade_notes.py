#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Checks the upgrade notes in upgrade-notes/.

- every release tag (vX.Y.Z) since the upgrade baseline 0.3.0 has
  upgrade-notes/X.Y.Z.toml (the 0.1/0.2 previews have none);
- every notes file has the required keys and a version matching its name;
- with --tag vX.Y.Z (the release workflow): that release's notes exist and
  its schema_version matches REQUIRED_SCHEMA in crates/db/src/lib.rs.

The binary parses the same files strictly (apps/slath/src/upgrade_notes.rs).

Usage: check_upgrade_notes.py [--tag TAG] [ROOT]
"""
import re
import subprocess
import sys
import tomllib
from pathlib import Path

TAG = re.compile(r"^v(\d+\.\d+\.\d+(?:-[0-9A-Za-z.]+)?)$")
REQUIRED = {"version", "schema_version", "min_upgrade_from"}
SEVERITIES = {"required", "recommended"}
BASELINE = (0, 3, 0)


def before_baseline(version: str) -> bool:
    core = version.split("-", 1)[0]
    return tuple(int(p) for p in core.split(".")) < BASELINE


def tags(root: Path) -> list[str]:
    out = subprocess.run(
        ["git", "-C", str(root), "tag", "--list", "v*"],
        capture_output=True, text=True, check=False,
    )
    return [t for t in out.stdout.split() if TAG.match(t)]


def check_file(path: Path) -> list[str]:
    try:
        data = tomllib.loads(path.read_text(encoding="utf-8"))
    except (OSError, tomllib.TOMLDecodeError) as e:
        return [f"{path.name}: {e}"]
    errors = []
    missing = REQUIRED - data.keys()
    if missing:
        errors.append(f"{path.name}: missing {', '.join(sorted(missing))}")
    if data.get("version") != path.stem:
        errors.append(f"{path.name}: version {data.get('version')!r} doesn't match the file name")
    elif before_baseline(path.stem):
        errors.append(f"{path.name}: releases before 0.3.0 were previews and have no upgrade notes")
    for a in data.get("action", []):
        if a.get("severity") not in SEVERITIES or not a.get("id") or not a.get("text"):
            errors.append(f"{path.name}: every [[action]] needs id, text, and severity required|recommended")
    return errors


def required_schema(root: Path) -> int | None:
    m = re.search(r"REQUIRED_SCHEMA: i32 = (\d+);", (root / "crates/db/src/lib.rs").read_text())
    return int(m.group(1)) if m else None


def check(root: Path, tag: str | None = None, release_tags: list[str] | None = None) -> list[str]:
    notes = root / "upgrade-notes"
    errors = []
    for f in sorted(notes.glob("*.toml")):
        errors += check_file(f)
    for t in release_tags if release_tags is not None else tags(root):
        version = TAG.match(t).group(1)
        if before_baseline(version):
            continue
        if not (notes / f"{version}.toml").is_file():
            errors.append(f"release {t} has no upgrade-notes/{version}.toml")
    if tag:
        m = TAG.match(tag)
        if not m:
            return errors + [f"{tag} is not a release tag (vX.Y.Z)"]
        f = notes / f"{m.group(1)}.toml"
        if not f.is_file():
            errors.append(f"release {tag} has no upgrade-notes/{m.group(1)}.toml")
        else:
            data = tomllib.loads(f.read_text(encoding="utf-8"))
            if data.get("schema_version") != required_schema(root):
                errors.append(f"{f.name}: schema_version must equal REQUIRED_SCHEMA ({required_schema(root)})")
    return sorted(set(errors))


def main() -> int:
    args = sys.argv[1:]
    tag = None
    if args[:1] == ["--tag"]:
        tag, args = args[1], args[2:]
    root = Path(args[0]) if args else Path(__file__).resolve().parent.parent
    errors = check(root, tag)
    for e in errors:
        print(f"upgrade-notes: {e}", file=sys.stderr)
    return 1 if errors else 0


if __name__ == "__main__":
    sys.exit(main())
