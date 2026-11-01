#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Reproducibly builds the bundled fonts (web/src/fonts/*.woff2 and
web/src/fonts/fonts.css) from pinned upstream releases.

1. Downloads each release archive and checks its SHA-256.
2. Source Sans 3 is copied unmodified (Adobe's own variable WOFF2 files):
   it has the Reserved Font Name "Source", and a subset would be a Modified
   Version that may not carry that name. JetBrains Mono has no reserved name
   and is subset to Latin + Latin Extended + punctuation + box drawing with
   pinned fontTools, keeping the weight axis.
3. Writes fonts.css: @font-face rules for the bundled faces and metric-matched
   fallback faces (size-adjust and ascent/descent/line-gap overrides computed
   from the font's metrics against Arial / Courier New), so swapping in the
   web font doesn't shift the layout.

Usage: scripts/fonts.py   (needs network access and python3 -m venv)
The output is committed; run this only to change fonts or subsets.
"""
import hashlib
import io
import os
import subprocess
import sys
import urllib.request
import zipfile
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
OUT = ROOT / "web/src/fonts"
VENV = ROOT / "target/fonts-venv"
FONTTOOLS = "fonttools==4.68.0"
BROTLI = "brotli==1.2.1"

RELEASES = {
    "source-sans": {
        "url": "https://github.com/adobe-fonts/source-sans/releases/download/3.052R/WOFF2-source-sans-3.052R.zip",
        "sha256": "d7f6724027fe5ca0bab44d6121284f1fa8f66dec5f2864aa969aed347041ac95",
    },
    "jetbrains-mono": {
        "url": "https://github.com/JetBrains/JetBrainsMono/releases/download/v2.304/JetBrainsMono-2.304.zip",
        "sha256": "6f6376c6ed2960ea8a963cd7387ec9d76e3f629125bc33d1fdcd7eb7012f7bbf",
    },
}

LATIN = (
    "U+0000-00FF,U+0100-024F,U+0259,U+02BB-02BC,U+02C6,U+02DA,U+02DC,U+0300-036F,"
    "U+1E00-1EFF,U+2000-206F,U+2070-209F,U+20A0-20CF,U+2100-214F,U+2190-21FF,"
    "U+2200-22FF,U+2C60-2C7F,U+A720-A7FF,U+FEFF,U+FFFD"
)
CODE_EXTRA = ",U+2300-23FF,U+2500-259F,U+25A0-25FF"

# (output, release, member in the archive, unicodes or None = copy unmodified,
#  family, style, fallback)
FACES = [
    ("SourceSans3-Upright.woff2", "source-sans", "WOFF2/VF/SourceSans3VF-Upright.ttf.woff2", None, "Source Sans 3", "normal", "sans"),
    ("SourceSans3-Italic.woff2", "source-sans", "WOFF2/VF/SourceSans3VF-Italic.ttf.woff2", None, "Source Sans 3", "italic", "sans"),
    ("JetBrainsMono-Upright.woff2", "jetbrains-mono", "fonts/variable/JetBrainsMono[wght].ttf", LATIN + CODE_EXTRA, "JetBrains Mono", "normal", "mono"),
    ("JetBrainsMono-Italic.woff2", "jetbrains-mono", "fonts/variable/JetBrainsMono-Italic[wght].ttf", LATIN + CODE_EXTRA, "JetBrains Mono", "italic", "mono"),
]

# Metrics of the local fallback fonts (unitsPerEm, xAvgCharWidth).
FALLBACKS = {
    "sans": {"local": ['local("Arial")', 'local("Liberation Sans")', 'local("Helvetica")'], "upm": 2048, "avg": 904},
    "mono": {"local": ['local("Courier New")', 'local("Liberation Mono")', 'local("Courier")'], "upm": 2048, "avg": 1229},
}


def fetch(release: str) -> zipfile.ZipFile:
    r = RELEASES[release]
    cache = ROOT / "target/fonts-cache" / Path(r["url"]).name
    if not cache.exists():
        cache.parent.mkdir(parents=True, exist_ok=True)
        with urllib.request.urlopen(r["url"]) as resp:  # noqa: S310 (pinned https URL)
            cache.write_bytes(resp.read())
    data = cache.read_bytes()
    digest = hashlib.sha256(data).hexdigest()
    if digest != r["sha256"]:
        cache.unlink()
        sys.exit(f"fonts: {r['url']}: SHA-256 {digest} does not match the pinned {r['sha256']}")
    return zipfile.ZipFile(io.BytesIO(data))


def venv_python() -> Path:
    py = VENV / "bin/python"
    if not py.exists():
        subprocess.run([sys.executable, "-m", "venv", str(VENV)], check=True)
        subprocess.run([str(py), "-m", "pip", "install", "-q", FONTTOOLS, BROTLI], check=True)
    return py


# Average character width with the OS/2 version 1 letter-frequency weights,
# the method the fallback fonts' xAvgCharWidth values above were computed with.
METRICS_SCRIPT = """
import json, sys
from fontTools.ttLib import TTFont
W = dict(a=64, b=14, c=27, d=35, e=100, f=20, g=14, h=42, i=63, j=3, k=6, l=35, m=20,
         n=56, o=56, p=17, q=4, r=49, s=56, t=71, u=31, v=10, w=18, x=3, y=18, z=2)
W[" "] = 166
f = TTFont(sys.argv[1])
cmap, hmtx, hhea = f.getBestCmap(), f["hmtx"], f["hhea"]
avg = sum(hmtx[cmap[ord(c)]][0] * w for c, w in W.items()) / 1000
print(json.dumps({"upm": f["head"].unitsPerEm, "avg": avg,
                  "asc": hhea.ascent, "desc": -hhea.descent, "gap": hhea.lineGap}))
"""


def main() -> int:
    py = venv_python()
    OUT.mkdir(parents=True, exist_ok=True)
    tmp = ROOT / "target/fonts-src"
    tmp.mkdir(parents=True, exist_ok=True)
    faces_css = []
    fallbacks: dict[str, dict] = {}
    for out, release, member, unicodes, family, style, kind in FACES:
        src = tmp / Path(member).name
        src.write_bytes(fetch(release).read(member))
        if unicodes is None:
            (OUT / out).write_bytes(src.read_bytes())
        else:
            subprocess.run(
                [str(py), "-m", "fontTools.subset", str(src), f"--unicodes={unicodes}",
                 "--flavor=woff2", f"--output-file={OUT / out}", "--layout-features=*",
                 "--no-hinting", "--desubroutinize", "--name-IDs=*", "--name-languages=*"],
                check=True, env={**os.environ, "SOURCE_DATE_EPOCH": "0"},
            )
        faces_css.append(
            f'@font-face {{\n  font-family: "{family}";\n  font-style: {style};\n  font-weight: 200 900;\n'
            f'  font-display: swap;\n  src: url("./{out}") format("woff2");\n}}'
        )
        if style == "normal":
            import json
            m = json.loads(subprocess.run([str(py), "-c", METRICS_SCRIPT, str(OUT / out)], check=True,
                                          capture_output=True, text=True).stdout)
            fb = FALLBACKS[kind]
            size = (m["avg"] / m["upm"]) / (fb["avg"] / fb["upm"])
            fallbacks[family] = {
                "local": fb["local"], "size": size,
                "asc": m["asc"] / m["upm"] / size, "desc": m["desc"] / m["upm"] / size, "gap": m["gap"] / m["upm"] / size,
            }
    for family, f in fallbacks.items():
        faces_css.append(
            f'@font-face {{\n  font-family: "{family} Fallback";\n  src: {", ".join(f["local"])};\n'
            f'  size-adjust: {f["size"] * 100:.2f}%;\n  ascent-override: {f["asc"] * 100:.2f}%;\n'
            f'  descent-override: {f["desc"] * 100:.2f}%;\n  line-gap-override: {f["gap"] * 100:.2f}%;\n}}'
        )
    header = (
        "/* SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de> */\n"
        "/* SPDX-License-Identifier: AGPL-3.0-or-later */\n"
        "/* Generated by scripts/fonts.py. Do not edit. The font files are OFL-1.1 (THIRD-PARTY-NOTICES.md). */\n\n"
    )
    (OUT / "fonts.css").write_text(header + "\n\n".join(faces_css) + "\n", encoding="utf-8")
    for out, *_ in FACES:
        print(f"fonts: {out} {(OUT / out).stat().st_size} bytes sha256 {hashlib.sha256((OUT / out).read_bytes()).hexdigest()}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
