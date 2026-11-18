// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//
// Accessibility, including contrast, of every page built from the
// components and of the design catalog, checked with axe. Contrast is what
// the color theme changes, so this runs in every cell, the dark ones too,
// where the behavior tests don't run. Pages go in groups of eight, so one
// sign-in serves several pages.
import { expectAccessible, test } from "../fixtures";
import { CATALOG, MIGRATED, type Account, openPage } from "./pages";

const PAGES = [...CATALOG.map((p) => `anon:${p}`), ...MIGRATED];
const GROUP = 8;

for (let i = 0; i < PAGES.length; i += GROUP) {
  const group = PAGES.slice(i, i + GROUP);
  test(`conf_a11y_01_pages_are_accessible ${i / GROUP + 1}: ${group[0]} …`, async ({ page }) => {
    // Eight pages, each loaded and checked: more than one page's time.
    test.slow();
    let signedIn: Account | undefined;
    for (const path of group) {
      await test.step(path, async () => {
        signedIn = await openPage(page, path, "contrast", signedIn);
        await expectAccessible(page);
      });
    }
  });
}
