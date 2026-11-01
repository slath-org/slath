// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
import { expect, expectAccessible, test } from "../fixtures";

test("conf_a11y_01_error_page_accessible", async ({ page }) => {
  const res = await page.goto("/-/no-such-page");
  expect(res?.status()).toBe(404);
  await expect(page.getByRole("heading", { level: 1 })).toHaveText("Not found");
  await expect(page).toHaveTitle("Not found · Slath");
  await expectAccessible(page);
});

test("conf_a11y_01_skip_link_moves_focus_to_main", async ({ page }) => {
  await page.goto("/-/no-such-page");
  await page.keyboard.press("Tab");
  const skip = page.getByRole("link", { name: "Skip to main content" });
  await expect(skip).toBeFocused();
  await page.keyboard.press("Enter");
  await expect(page.locator("main")).toBeFocused();
});
