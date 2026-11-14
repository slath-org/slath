// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//
// The frame: the global bar is one row at every width, with the account
// menu on narrow screens; context headers.
import { USERS, expect, expectAccessible, navLink, signIn, tapOrClick, test } from "../fixtures";

test("conf_design_03_global_bar_is_one_row", async ({ page }) => {
  await signIn(page, USERS.admin);
  await page.goto("/-/design/frame");
  const bar = page.locator(".site-nav");
  const h = await bar.evaluate((el) => el.getBoundingClientRect().height);
  const row = await page.locator(".site-name").evaluate((el) => el.getBoundingClientRect().height);
  // One row: no taller than its tallest control plus the bar's padding.
  expect(h).toBeLessThan(row * 2);
  await expectAccessible(page);
});

test("conf_design_03_account_menu_on_narrow_screens", async ({ page }) => {
  await signIn(page, USERS.admin);
  await page.goto("/-/design/frame");
  const narrow = (test.info().project.use.viewport?.width ?? 1280) < 640;
  const menu = page.locator("details.account-menu");
  await expect(menu).toBeVisible({ visible: narrow });
  const admin = await navLink(page, "Admin");
  await expect(admin).toBeVisible();
  if (narrow) {
    await expect(menu.getByRole("radio", { name: "Dark" })).toBeVisible();
    await expectAccessible(page, "details.account-menu .menu-panel");
    await page.keyboard.press("Escape");
    await expect(menu).not.toHaveAttribute("open", "");
  }
  await tapOrClick(page, await navLink(page, "Admin"));
  await expect(page.getByRole("heading", { level: 1 })).toHaveText("Users");
});

test("conf_design_03_context_header", async ({ page }) => {
  await page.goto("/-/design/frame");
  const header = page.locator(".context-header").first();
  await expect(header.getByRole("link", { name: "acme" })).toHaveAttribute("href", "/acme");
  await expect(header.locator(".badge")).toHaveText("Organization");
  await expect(header.locator(".avatar")).toHaveCount(1);
  await expectAccessible(page);
});
