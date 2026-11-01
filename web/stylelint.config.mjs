// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//
// Weave restraint rules: components use design tokens, never raw colors,
// font sizes, or spacing values. Raw values live only in tokens.css (and
// the @font-face metrics in fonts.css).

const raw = "/^-?\\d*\\.?\\d+(px|rem|em)$|\\s-?\\d*\\.?\\d+(px|rem|em)/";

export default {
  ignoreFiles: ["src/tokens.css", "src/fonts/fonts.css", "dist/**", "node_modules/**"],
  rules: {
    "color-no-hex": true,
    "color-named": "never",
    "function-disallowed-list": ["rgb", "rgba", "hsl", "hsla", "hwb", "lab", "lch", "oklab", "oklch", "color", "color-mix"],
    "declaration-property-value-disallowed-list": {
      "/^font-size$/": ["/\\d/"],
      "/^(margin|padding|gap|row-gap|column-gap)(-.*)?$/": [raw],
      "/^(font-family)$/": ["/^(?!var\\().*/"],
      // Shadows come from tokens (elevation or an accent indicator).
      "/^(box-shadow)$/": ["/^(?!none$)(?!.*var\\(--).*/"],
    },
    "declaration-no-important": true,
    "length-zero-no-unit": true,
  },
};
