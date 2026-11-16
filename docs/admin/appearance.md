<!--
SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
SPDX-License-Identifier: AGPL-3.0-or-later
-->

# Instance logo

Every page shows a logo in its header and as the browser tab's icon: the
Slath logo, until a site administrator uploads the instance's own under
**Administration → Appearance**. There's one image for the light theme
and one for the dark theme; upload either or both. Pages pick the one for
the viewer's theme, or the device's when the viewer follows it.

## Uploading

- **Formats:** SVG, PNG, or WebP, at most `limits.logo_max_bytes`
  (1 MiB by default). The header shows the logo in a square, so a square
  image fits best; other shapes are fitted inside it.
- **SVG** files are stored as uploaded, but only if they just draw:
  shapes, paths, text, gradients, groups, clip paths, and masks, with their
  presentation attributes and inline `style`. Anything that could run or
  load something is refused with the reason: scripts, event handlers
  (`onload`, …), `<style>` sheets, `<foreignObject>`, `<image>`, links,
  animation, `href` or `url()` values that point outside the file, and
  DTDs or entities. Editors' own markup (for example Inkscape's
  `inkscape:` attributes) is refused too; save as a plain SVG (in Inkscape,
  **Plain SVG**, or run it through `svgo`).
- **PNG and WebP** images are decoded and encoded again as PNG, scaled to
  at most 512 pixels on the longer side. Only the pixels survive: metadata
  and anything appended to the file are dropped.

Logos are served from `/-/logos/<sha256>.svg` (or `.png`) on the instance
itself, with a sandboxing content security policy; pages never load
anything from elsewhere.

## Going back

**Use the Slath logo again** removes both uploaded images. Uploading,
and going back, are recorded in the audit log (`admin.logo.set`,
`admin.logo.reset`), and need a recent sign-in like other administration
changes.

Backups include the logos; a restored instance shows the logo it had.
