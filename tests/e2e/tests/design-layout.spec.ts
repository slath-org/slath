// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//
// The catalog's layout family: distances by relationship in both
// densities, and the headings' space.
import { type Page } from "@playwright/test";
import { expect, expectAccessible, test } from "../fixtures";

/** The vertical gaps between the children of the element matching `sel`. */
async function gaps(page: Page, sel: string): Promise<number[]> {
  return page.locator(sel).first().evaluate((el) => {
    const kids = [...el.children];
    const out: number[] = [];
    for (let i = 1; i < kids.length; i++) {
      out.push(Math.round(kids[i].getBoundingClientRect().top - kids[i - 1].getBoundingClientRect().bottom));
    }
    return out;
  });
}

async function density(page: Page, d: "comfortable" | "compact"): Promise<void> {
  await page.evaluate((v) => {
    document.documentElement.dataset.density = v;
  }, d);
}

test("conf_design_03_layout_family_accessible", async ({ page }) => {
  await page.goto("/-/design/layout");
  await expect(page.getByRole("heading", { level: 1 })).toHaveText("Layout");
  await expectAccessible(page);
  await density(page, "compact");
  await expectAccessible(page);
});

test("conf_design_03_layout_distances_by_relationship", async ({ page }) => {
  await page.goto("/-/design/layout");
  const expected = {
    comfortable: { items: 16, groups: 24, inside: 4, sections: 32 },
    compact: { items: 12, groups: 16, inside: 4, sections: 24 },
  };
  for (const d of ["comfortable", "compact"] as const) {
    await density(page, d);
    const e = expected[d];
    expect(await gaps(page, "section[aria-labelledby=stack] figure .stack")).toEqual([e.items, e.items]);
    expect(await gaps(page, "section[aria-labelledby=stack] .stack-groups")).toEqual([e.groups]);
    expect(await gaps(page, "section[aria-labelledby=stack] .stack-inside")).toEqual([e.inside, e.inside]);
    // Catalog entries are sections of the page.
    expect((await gaps(page, "main .page")).every((g) => g === e.sections)).toBe(true);
    // A section's heading sits 12 px above its content, closer than the
    // content's items, in both densities.
    const [headToContent, ...items] = await gaps(page, "section[aria-labelledby=demo-section-1]");
    expect(headToContent).toBe(12);
    expect(items).toEqual([e.items]);
  }
});

test("conf_design_03_frame_title_and_padding", async ({ page }) => {
  await page.goto("/-/design/layout");
  const frame = page.locator("section[aria-labelledby=demo-frame-1]");
  const m = await frame.evaluate((el) => {
    const s = getComputedStyle(el);
    const title = el.querySelector(".frame-title")!.getBoundingClientRect();
    const next = el.querySelector(".frame-title + *")!.getBoundingClientRect();
    return { padding: s.paddingTop, titleGap: Math.round(next.top - title.bottom) };
  });
  expect(m.padding).toBe("16px");
  expect(m.titleGap).toBe(8);
});
