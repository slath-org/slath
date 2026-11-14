// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//
// The catalog's lists family: tables of records stack on phones, so no
// value or action is off screen.
import { expect, expectAccessible, test } from "../fixtures";

test("conf_design_03_lists_family_accessible", async ({ page }) => {
  await page.goto("/-/design/lists");
  await expect(page.getByRole("heading", { level: 1 })).toHaveText("Lists and data");
  await expectAccessible(page);
});

test("conf_design_03_table_of_records_stacks_when_narrow", async ({ page }) => {
  await page.goto("/-/design/lists");
  const table = page.locator(".table-records");
  const narrow = (test.info().project.use.viewport?.width ?? 1280) < 640;
  const display = await table.locator("tbody tr").first().evaluate((tr) => getComputedStyle(tr).display);
  expect(display).toBe(narrow ? "block" : "table-row");
  // Every action is on screen without scrolling the table.
  const vw = await page.evaluate(() => document.documentElement.clientWidth);
  for (const b of await table.getByRole("button").all()) {
    const box = (await b.boundingBox())!;
    expect(box.x + box.width).toBeLessThanOrEqual(vw);
  }
  if (narrow) {
    // Column names show as labels.
    const label = await table.locator("td[data-label=Scopes]").first().evaluate((td) => getComputedStyle(td, "::before").content);
    expect(label).toBe('"Scopes"');
  }
});

test("conf_design_03_pager_and_more", async ({ page }) => {
  await page.goto("/-/design/lists");
  const pager = page.getByRole("navigation", { name: "Pages" });
  await expect(pager.getByRole("link", { name: "Previous page" })).toHaveAttribute("rel", "prev");
  await expect(pager.getByRole("link", { name: "Next page" })).toHaveAttribute("rel", "next");
  await expect(page.getByRole("link", { name: "and 7 more" })).toBeVisible();
});
