#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Fail if an Apache-2.0 workspace crate depends on an AGPL workspace crate.

Relicensing AGPL code needs every contributor's consent, so permissively licensed
crates (client SDKs, runner protocol, the v1.x runner) must never link AGPL code.
Checks normal and build dependencies transitively; dev-dependencies are exempt
because they don't end up in the distributed artifact.

Usage: check_license_deps.py [--manifest-path Cargo.toml]
"""
from __future__ import annotations

import json
import subprocess
import sys


def load_metadata(manifest_path: str | None) -> dict:
    cmd = ["cargo", "metadata", "--format-version", "1", "--offline"]
    if manifest_path:
        cmd += ["--manifest-path", manifest_path]
    out = subprocess.run(cmd, check=True, capture_output=True, text=True).stdout
    return json.loads(out)


def is_agpl(license_expr: str | None) -> bool:
    return bool(license_expr) and "AGPL" in license_expr


def is_permissive_only(license_expr: str | None) -> bool:
    """True for crates that must stay free of AGPL code (e.g. `Apache-2.0`)."""
    return bool(license_expr) and not is_agpl(license_expr) and "GPL" not in license_expr


def violations(meta: dict) -> list[str]:
    members = set(meta["workspace_members"])
    pkgs = {p["id"]: p for p in meta["packages"] if p["id"] in members}
    by_name = {p["name"]: p for p in pkgs.values()}

    def runtime_deps(pkg: dict) -> list[dict]:
        out = []
        for dep in pkg["dependencies"]:
            if dep.get("kind") == "dev" or dep.get("path") is None:
                continue
            target = by_name.get(dep["name"])
            if target is not None:
                out.append(target)
        return out

    errors = []
    for pkg in sorted(pkgs.values(), key=lambda p: p["name"]):
        if not is_permissive_only(pkg.get("license")):
            continue
        seen: set[str] = set()
        stack = [(d, [pkg["name"], d["name"]]) for d in runtime_deps(pkg)]
        while stack:
            dep, chain = stack.pop()
            if dep["name"] in seen:
                continue
            seen.add(dep["name"])
            if is_agpl(dep.get("license")):
                errors.append(
                    f"{pkg['name']} ({pkg['license']}) depends on {dep['name']} "
                    f"({dep['license']}) via {' -> '.join(chain)}"
                )
            stack.extend((d, chain + [d["name"]]) for d in runtime_deps(dep))
    return errors


def main(argv: list[str]) -> int:
    manifest = None
    if len(argv) == 2 and argv[0] == "--manifest-path":
        manifest = argv[1]
    elif argv:
        print(__doc__, file=sys.stderr)
        return 2
    errors = violations(load_metadata(manifest))
    for e in errors:
        print(f"license-deps: {e}", file=sys.stderr)
    if not errors:
        print("license-deps: ok")
    return 1 if errors else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
