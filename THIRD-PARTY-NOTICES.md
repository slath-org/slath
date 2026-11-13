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
| invokers-polyfill | 1.0.4 | MIT ([text](LICENSES/MIT.txt)) | Copyright (c) 2025 Keith Cirkel | `invokers-polyfill.js`, loaded only by browsers without invoker commands | https://github.com/keithamus/invokers-polyfill, npm `invokers-polyfill` |
| Milkdown | 7.23.0 | MIT ([text](LICENSES/MIT.txt)) | Copyright (c) 2021 Mirone | the rich-text editor in `wysiwyg.js` (npm `@milkdown/kit`; its parts are listed below) | https://milkdown.dev |
| @milkdown/kit | 7.23.0 | MIT ([text](LICENSES/MIT.txt)) | Copyright (c) 2021 Mirone | `wysiwyg.js` | npm `@milkdown/kit` |
| @codemirror/commands | 6.11.1 | MIT ([text](LICENSES/MIT.txt)) | Copyright (C) 2018-2021 by Marijn Haverbeke and others | `source.js` | https://codemirror.net |
| @codemirror/lang-markdown | 6.5.2 | MIT ([text](LICENSES/MIT.txt)) | Copyright (C) 2018-2021 by Marijn Haverbeke and others | `source.js` | https://codemirror.net |
| @codemirror/language | 6.13.1 | MIT ([text](LICENSES/MIT.txt)) | Copyright (C) 2018-2021 by Marijn Haverbeke and others | `source.js` | https://codemirror.net |
| @codemirror/state | 6.7.6 | MIT ([text](LICENSES/MIT.txt)) | Copyright (C) 2018-2021 by Marijn Haverbeke and others | `source.js` | https://codemirror.net |
| @codemirror/view | 6.43.15 | MIT ([text](LICENSES/MIT.txt)) | Copyright (C) 2018-2021 by Marijn Haverbeke and others | `source.js` | https://codemirror.net |
| @lezer/highlight | 1.2.5 | MIT ([text](LICENSES/MIT.txt)) | Copyright (C) 2018 by Marijn Haverbeke and others | `source.js` | https://lezer.codemirror.net |

`scripts/fonts.py` downloads the pinned release archives and checks their
SHA-256. Source Sans 3 is shipped unmodified, because its license reserves
the font name "Source" and a modified version would have to be renamed.
JetBrains Mono, which has no reserved font name, is subset to the Latin
scripts, punctuation, and box drawing.

## Packages in the Markdown editor chunks

The editor is loaded only on pages with a Markdown field: `shell.js` (Slath's
own code), `wysiwyg.js` (rich text), `source.js` (Markdown source), and
`w3c-keyname.js` (shared). Source maps (`*.js.map`) cover the same code. Every
package bundled into them, with the copyright line of its license file; all
are under the MIT license ([text](LICENSES/MIT.txt)).

| Package | Version | License | Copyright | In |
|---|---|---|---|---|
| @codemirror/commands | 6.11.1 | MIT | Copyright (C) 2018-2021 by Marijn Haverbeke <marijn@haverbeke.berlin> and others | `source.js` |
| @codemirror/lang-markdown | 6.5.2 | MIT | Copyright (C) 2018-2021 by Marijn Haverbeke <marijn@haverbeke.berlin> and others | `source.js` |
| @codemirror/language | 6.13.1 | MIT | Copyright (C) 2018-2021 by Marijn Haverbeke <marijn@haverbeke.berlin> and others | `source.js` |
| @codemirror/state | 6.7.6 | MIT | Copyright (C) 2018-2021 by Marijn Haverbeke <marijn@haverbeke.berlin> and others | `source.js` |
| @codemirror/view | 6.43.15 | MIT | Copyright (C) 2018-2021 by Marijn Haverbeke <marijn@haverbeke.berlin> and others | `source.js` |
| @lezer/common | 1.5.5 | MIT | Copyright (C) 2018 by Marijn Haverbeke <marijn@haverbeke.berlin> and others | `source.js` |
| @lezer/highlight | 1.2.5 | MIT | Copyright (C) 2018 by Marijn Haverbeke <marijn@haverbeke.berlin> and others | `source.js` |
| @lezer/markdown | 1.8.1 | MIT | Copyright (C) 2020 by Marijn Haverbeke <marijn@haverbeke.berlin> and others | `source.js` |
| @marijn/find-cluster-break | 1.0.4 | MIT | Copyright (C) 2024 by Marijn Haverbeke <marijn@haverbeke.berlin> | `source.js` |
| @milkdown/core | 7.23.0 | MIT | Copyright (c) 2020-present Mirone | `wysiwyg.js` |
| @milkdown/ctx | 7.23.0 | MIT | Copyright (c) 2020-present Mirone | `wysiwyg.js` |
| @milkdown/exception | 7.23.0 | MIT | Copyright (c) 2020-present Mirone | `wysiwyg.js` |
| @milkdown/plugin-history | 7.23.0 | MIT | Copyright (c) 2020-present Mirone | `wysiwyg.js` |
| @milkdown/plugin-listener | 7.23.0 | MIT | Copyright (c) 2020-present Mirone | `wysiwyg.js` |
| @milkdown/preset-commonmark | 7.23.0 | MIT | Copyright (c) 2020-present Mirone | `wysiwyg.js` |
| @milkdown/preset-gfm | 7.23.0 | MIT | Copyright (c) 2020-present Mirone | `wysiwyg.js` |
| @milkdown/prose | 7.23.0 | MIT | Copyright (c) 2020-present Mirone | `wysiwyg.js` |
| @milkdown/transformer | 7.23.0 | MIT | Copyright (c) 2020-present Mirone | `wysiwyg.js` |
| @milkdown/utils | 7.23.0 | MIT | Copyright (c) 2020-present Mirone | `wysiwyg.js` |
| bail | 2.0.2 | MIT | Copyright (c) 2015 Titus Wormer <tituswormer@gmail.com> | `wysiwyg.js` |
| ccount | 2.0.1 | MIT | Copyright (c) 2015 Titus Wormer <tituswormer@gmail.com> | `wysiwyg.js` |
| devlop | 1.1.0 | MIT | Copyright (c) 2023 Titus Wormer <tituswormer@gmail.com> | `wysiwyg.js` |
| escape-string-regexp | 5.0.0 | MIT | Copyright (c) Sindre Sorhus <sindresorhus@gmail.com> (https://sindresorhus.com) | `wysiwyg.js` |
| extend | 3.0.2 | MIT | Copyright (c) 2014 Stefan Thomas | `wysiwyg.js` |
| is-plain-obj | 4.1.0 | MIT | Copyright (c) Sindre Sorhus <sindresorhus@gmail.com> (https://sindresorhus.com) | `wysiwyg.js` |
| lodash-es | 4.18.1 | MIT | Copyright OpenJS Foundation and other contributors <https://openjsf.org/> | `wysiwyg.js` |
| longest-streak | 3.1.0 | MIT | Copyright (c) 2015 Titus Wormer <mailto:tituswormer@gmail.com> | `wysiwyg.js` |
| markdown-table | 3.0.4 | MIT | Copyright (c) Titus Wormer <tituswormer@gmail.com> | `wysiwyg.js` |
| mdast-util-definitions | 6.0.0 | MIT | Copyright (c) 2015-2016 Titus Wormer <tituswormer@gmail.com> | `wysiwyg.js` |
| mdast-util-find-and-replace | 3.0.4 | MIT | Copyright (c) Titus Wormer <tituswormer@gmail.com> | `wysiwyg.js` |
| mdast-util-from-markdown | 2.1.0 | MIT | Copyright (c) Titus Wormer <tituswormer@gmail.com> | `wysiwyg.js` |
| mdast-util-gfm | 3.1.0 | MIT | Copyright (c) Titus Wormer <tituswormer@gmail.com> | `wysiwyg.js` |
| mdast-util-gfm-autolink-literal | 2.0.1 | MIT | Copyright (c) 2020 Titus Wormer <tituswormer@gmail.com> | `wysiwyg.js` |
| mdast-util-gfm-footnote | 2.1.0 | MIT | Copyright (c) Titus Wormer <tituswormer@gmail.com> | `wysiwyg.js` |
| mdast-util-gfm-strikethrough | 2.0.1 | MIT | Copyright (c) Titus Wormer <tituswormer@gmail.com> | `wysiwyg.js` |
| mdast-util-gfm-table | 2.0.0 | MIT | Copyright (c) 2020 Titus Wormer <tituswormer@gmail.com> | `wysiwyg.js` |
| mdast-util-gfm-task-list-item | 2.0.0 | MIT | Copyright (c) 2020 Titus Wormer <tituswormer@gmail.com> | `wysiwyg.js` |
| mdast-util-phrasing | 4.1.0 | MIT | Copyright (c) 2017 Titus Wormer <tituswormer@gmail.com> | `wysiwyg.js` |
| mdast-util-to-markdown | 2.2.1 | MIT | Copyright (c) Titus Wormer <tituswormer@gmail.com> | `wysiwyg.js` |
| mdast-util-to-string | 4.0.0 | MIT | Copyright (c) 2015 Titus Wormer <tituswormer@gmail.com> | `wysiwyg.js` |
| micromark | 4.0.3 | MIT | Copyright (c) Titus Wormer <tituswormer@gmail.com> | `wysiwyg.js` |
| micromark-core-commonmark | 2.0.4 | MIT | Copyright (c) Titus Wormer <tituswormer@gmail.com> | `wysiwyg.js` |
| micromark-extension-gfm | 3.0.0 | MIT | Copyright (c) 2020 Titus Wormer <tituswormer@gmail.com> | `wysiwyg.js` |
| micromark-extension-gfm-autolink-literal | 2.1.0 | MIT | Copyright (c) 2020 Titus Wormer <tituswormer@gmail.com> | `wysiwyg.js` |
| micromark-extension-gfm-footnote | 2.1.0 | MIT | Copyright (c) 2021 Titus Wormer <tituswormer@gmail.com> | `wysiwyg.js` |
| micromark-extension-gfm-strikethrough | 2.1.0 | MIT | Copyright (c) 2020 Titus Wormer <tituswormer@gmail.com> | `wysiwyg.js` |
| micromark-extension-gfm-table | 2.1.2 | MIT | Copyright (c) Titus Wormer <tituswormer@gmail.com> | `wysiwyg.js` |
| micromark-extension-gfm-task-list-item | 2.1.0 | MIT | Copyright (c) 2020 Titus Wormer <tituswormer@gmail.com> | `wysiwyg.js` |
| micromark-factory-destination | 2.0.1 | MIT | Copyright (c) Titus Wormer <tituswormer@gmail.com> | `wysiwyg.js` |
| micromark-factory-label | 2.0.1 | MIT | Copyright (c) Titus Wormer <tituswormer@gmail.com> | `wysiwyg.js` |
| micromark-factory-space | 2.1.0 | MIT | Copyright (c) Titus Wormer <tituswormer@gmail.com> | `wysiwyg.js` |
| micromark-factory-title | 2.0.1 | MIT | Copyright (c) Titus Wormer <tituswormer@gmail.com> | `wysiwyg.js` |
| micromark-factory-whitespace | 2.0.1 | MIT | Copyright (c) Titus Wormer <tituswormer@gmail.com> | `wysiwyg.js` |
| micromark-util-character | 2.1.1 | MIT | Copyright (c) Titus Wormer <tituswormer@gmail.com> | `wysiwyg.js` |
| micromark-util-chunked | 2.0.1 | MIT | Copyright (c) Titus Wormer <tituswormer@gmail.com> | `wysiwyg.js` |
| micromark-util-classify-character | 2.0.1 | MIT | Copyright (c) Titus Wormer <tituswormer@gmail.com> | `wysiwyg.js` |
| micromark-util-combine-extensions | 2.0.1 | MIT | Copyright (c) Titus Wormer <tituswormer@gmail.com> | `wysiwyg.js` |
| micromark-util-decode-numeric-character-reference | 2.0.2 | MIT | Copyright (c) Titus Wormer <tituswormer@gmail.com> | `wysiwyg.js` |
| micromark-util-decode-string | 2.0.1 | MIT | Copyright (c) Titus Wormer <tituswormer@gmail.com> | `wysiwyg.js` |
| micromark-util-edit-map | 1.0.0 | MIT | Copyright (c) Titus Wormer <tituswormer@gmail.com> | `wysiwyg.js` |
| micromark-util-html-tag-name | 2.0.1 | MIT | Copyright (c) Titus Wormer <tituswormer@gmail.com> | `wysiwyg.js` |
| micromark-util-normalize-identifier | 2.0.1 | MIT | Copyright (c) Titus Wormer <tituswormer@gmail.com> | `wysiwyg.js` |
| micromark-util-resolve-all | 2.0.1 | MIT | Copyright (c) Titus Wormer <tituswormer@gmail.com> | `wysiwyg.js` |
| micromark-util-subtokenize | 2.1.0 | MIT | Copyright (c) Titus Wormer <tituswormer@gmail.com> | `wysiwyg.js` |
| nanoid | 3.3.20 | MIT | Copyright 2017 Andrey Sitnik <andrey@sitnik.ru> | `wysiwyg.js` |
| orderedmap | 2.1.1 | MIT | Copyright (C) 2016 by Marijn Haverbeke <marijn@haverbeke.berlin> and others | `wysiwyg.js` |
| prosemirror-commands | 1.7.2 | MIT | Copyright (C) 2015-2017 by Marijn Haverbeke <marijn@haverbeke.berlin> and others | `wysiwyg.js` |
| prosemirror-history | 1.5.1 | MIT | Copyright (C) 2015-2017 by Marijn Haverbeke <marijn@haverbeke.berlin> and others | `wysiwyg.js` |
| prosemirror-inputrules | 1.5.1 | MIT | Copyright (C) 2015-2017 by Marijn Haverbeke <marijn@haverbeke.berlin> and others | `wysiwyg.js` |
| prosemirror-keymap | 1.2.3 | MIT | Copyright (C) 2015-2017 by Marijn Haverbeke <marijn@haverbeke.berlin> and others | `wysiwyg.js` |
| prosemirror-model | 1.25.12 | MIT | Copyright (C) 2015-2017 by Marijn Haverbeke <marijn@haverbeke.berlin> and others | `wysiwyg.js` |
| prosemirror-safari-ime-span | 1.0.2 | MIT | Copyright (c) 2024 ocavue | `wysiwyg.js` |
| prosemirror-schema-list | 1.6.0 | MIT | Copyright (C) 2015-2017 by Marijn Haverbeke <marijn@haverbeke.berlin> and others | `wysiwyg.js` |
| prosemirror-state | 1.4.4 | MIT | Copyright (C) 2015-2017 by Marijn Haverbeke <marijn@haverbeke.berlin> and others | `wysiwyg.js` |
| prosemirror-tables | 1.8.5 | MIT | Copyright (C) 2015-2016 by Marijn Haverbeke <marijnh@gmail.com> and others | `wysiwyg.js` |
| prosemirror-transform | 1.12.4 | MIT | Copyright (C) 2015-2017 by Marijn Haverbeke <marijn@haverbeke.berlin> and others | `wysiwyg.js` |
| prosemirror-view | 1.42.9 | MIT | Copyright (C) 2015-2017 by Marijn Haverbeke <marijn@haverbeke.berlin> and others | `wysiwyg.js` |
| remark-gfm | 4.0.1 | MIT | Copyright (c) Titus Wormer <tituswormer@gmail.com> | `wysiwyg.js` |
| remark-inline-links | 7.0.0 | MIT | Copyright (c) 2015 Titus Wormer <tituswormer@gmail.com> | `wysiwyg.js` |
| remark-parse | 11.0.0 | MIT | Copyright (c) 2014 Titus Wormer <tituswormer@gmail.com> | `wysiwyg.js` |
| remark-stringify | 11.0.0 | MIT | Copyright (c) 2014 Titus Wormer <tituswormer@gmail.com> | `wysiwyg.js` |
| rope-sequence | 1.3.4 | MIT | Copyright (C) 2016 by Marijn Haverbeke <marijn@haverbeke.berlin> | `wysiwyg.js` |
| style-mod | 4.1.4 | MIT | Copyright (C) 2018 by Marijn Haverbeke <marijn@haverbeke.berlin> and others | `source.js` |
| trough | 2.2.0 | MIT | Copyright (c) 2016 Titus Wormer <tituswormer@gmail.com> | `wysiwyg.js` |
| unified | 11.0.5 | MIT | Copyright (c) 2015 Titus Wormer <tituswormer@gmail.com> | `wysiwyg.js` |
| unist-util-is | 6.0.1 | MIT | Copyright (c) 2015 Titus Wormer <tituswormer@gmail.com> | `wysiwyg.js` |
| unist-util-stringify-position | 4.0.0 | MIT | Copyright (c) 2016 Titus Wormer <tituswormer@gmail.com> | `wysiwyg.js` |
| unist-util-visit | 5.2.0 | MIT | Copyright (c) Titus Wormer <tituswormer@gmail.com> | `wysiwyg.js` |
| unist-util-visit-parents | 6.1.0 | MIT | Copyright (c) Titus Wormer <tituswormer@gmail.com> | `wysiwyg.js` |
| vfile | 6.0.3 | MIT | Copyright (c) 2015 Titus Wormer <tituswormer@gmail.com> | `wysiwyg.js` |
| vfile-message | 4.0.3 | MIT | Copyright (c) Titus Wormer <tituswormer@gmail.com> | `wysiwyg.js` |
| w3c-keyname | 2.2.8 | MIT | Copyright (C) 2016 by Marijn Haverbeke <marijn@haverbeke.berlin> and others | `w3c-keyname.js` |
| zwitch | 2.0.4 | MIT | Copyright (c) 2016 Titus Wormer <tituswormer@gmail.com> | `wysiwyg.js` |

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
