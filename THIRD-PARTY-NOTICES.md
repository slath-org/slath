<!-- SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de> -->
<!-- SPDX-License-Identifier: CC-BY-SA-4.0 -->
# Third-party notices

The Slath web UI bundles the following third-party components. They're
served by the Slath instance itself; the UI loads nothing from elsewhere.
`scripts/check_asset_licenses.py` (part of `just check`) fails when a bundled
file or runtime dependency is missing from this list.

| Component | Version | License | Copyright | Bundled files | Source |
|---|---|---|---|---|---|
| Source Sans 3 | 3.052R | OFL-1.1 ([text](LICENSES/OFL-1.1.txt)) | Copyright 2010–2023 Adobe (http://www.adobe.com/), with Reserved Font Name "Source". | `SourceSans3-Upright.woff2`, `SourceSans3-Italic.woff2` (Adobe's variable WOFF2 files, unmodified) | https://github.com/adobe-fonts/source-sans/releases/tag/3.052R |
| JetBrains Mono | 2.304 | OFL-1.1 ([text](LICENSES/OFL-1.1.txt)) | Copyright 2020 The JetBrains Mono Project Authors (https://github.com/JetBrains/JetBrainsMono) | `JetBrainsMono-Upright.woff2`, `JetBrainsMono-Italic.woff2` (subsets of the variable fonts) | https://github.com/JetBrains/JetBrainsMono/releases/tag/v2.304 |
| Lucide | 1.66.0 | ISC ([text](LICENSES/ISC.txt)); icons derived from Feather: MIT (below) | Copyright (c) 2026 Lucide Icons and Contributors; Feather: Copyright (c) 2013-present Cole Bemis | `icons.svg` (sprite of the icons in `web/src/icons/icons.txt`) | https://lucide.dev, npm `lucide-static` |
| htmx | 4.0.0 | 0BSD ([text](LICENSES/0BSD.txt)) | Big Sky Software | part of `app.js` (with the `hx-csp` extension) | https://htmx.org, npm `htmx.org` |

`scripts/fonts.py` downloads the pinned release archives and checks their
SHA-256. Source Sans 3 is shipped unmodified, because its license reserves
the font name "Source" and a modified version would have to be renamed.
JetBrains Mono, which has no reserved font name, is subset to the Latin
scripts, punctuation, and box drawing.

## Lucide license (verbatim)

```text
ISC License

Copyright (c) 2026 Lucide Icons and Contributors

Permission to use, copy, modify, and/or distribute this software for any
purpose with or without fee is hereby granted, provided that the above
copyright notice and this permission notice appear in all copies.

THE SOFTWARE IS PROVIDED "AS IS" AND THE AUTHOR DISCLAIMS ALL WARRANTIES
WITH REGARD TO THIS SOFTWARE INCLUDING ALL IMPLIED WARRANTIES OF
MERCHANTABILITY AND FITNESS. IN NO EVENT SHALL THE AUTHOR BE LIABLE FOR
ANY SPECIAL, DIRECT, INDIRECT, OR CONSEQUENTIAL DAMAGES OR ANY DAMAGES
WHATSOEVER RESULTING FROM LOSS OF USE, DATA OR PROFITS, WHETHER IN AN
ACTION OF CONTRACT, NEGLIGENCE OR OTHER TORTIOUS ACTION, ARISING OUT OF
OR IN CONNECTION WITH THE USE OR PERFORMANCE OF THIS SOFTWARE.

---

The following Lucide icons are derived from the Feather project:

airplay, alert-circle, alert-octagon, alert-triangle, aperture, arrow-down-circle, arrow-down-left, arrow-down-right, arrow-down, arrow-left-circle, arrow-left, arrow-right-circle, arrow-right, arrow-up-circle, arrow-up-left, arrow-up-right, arrow-up, at-sign, calendar, cast, check, chevron-down, chevron-left, chevron-right, chevron-up, chevrons-down, chevrons-left, chevrons-right, chevrons-up, circle, clipboard, clock, code, columns, command, compass, corner-down-left, corner-down-right, corner-left-down, corner-left-up, corner-right-down, corner-right-up, corner-up-left, corner-up-right, crosshair, database, divide-circle, divide-square, dollar-sign, download, external-link, feather, frown, hash, headphones, help-circle, info, italic, key, layout, life-buoy, link-2, link, loader, lock, log-in, log-out, maximize, meh, minimize, minimize-2, minus-circle, minus-square, minus, monitor, moon, more-horizontal, more-vertical, move, music, navigation-2, navigation, octagon, pause-circle, percent, plus-circle, plus-square, plus, power, radio, rss, search, server, share, shopping-bag, sidebar, smartphone, smile, square, table-2, tablet, target, terminal, trash-2, trash, triangle, tv, type, upload, x-circle, x-octagon, x-square, x, zoom-in, zoom-out

The MIT License (MIT) (for the icons listed above)

Copyright (c) 2013-present Cole Bemis

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
```
