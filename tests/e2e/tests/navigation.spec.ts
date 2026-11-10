// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//
// The side menu of a context.
import { USERS, expect, expectAccessible, signIn, tapOrClick, test } from "../fixtures";

test("conf_a11y_04_side_menu_collapses_or_opens_as_a_drawer", async ({ page }) => {
  await signIn(page, USERS.alice);
  await page.goto("/");
  const menu = page.getByRole("navigation", { name: "Your work", exact: true });
  const toggle = page.getByRole("button", { name: "Side menu" });
  if (test.info().project.use.hasTouch) {
    // Narrow screens: a closed drawer the toggle opens and Escape closes.
    await expect(menu).toBeHidden();
    await expect(toggle).toHaveAttribute("aria-expanded", "false");
    await tapOrClick(page, toggle);
    await expect(menu).toBeVisible();
    await expect(toggle).toHaveAttribute("aria-expanded", "true");
    await expect(menu.getByRole("link", { name: "Explore" })).toBeFocused();
    await expectAccessible(page);
    await page.keyboard.press("Escape");
    await expect(menu).toBeHidden();
    await expect(toggle).toBeFocused();
    // The close button inside the drawer closes it too.
    await tapOrClick(page, toggle);
    await tapOrClick(page, menu.getByRole("link", { name: "Close side menu" }));
    await expect(menu).toBeHidden();
  } else {
    // Wide screens: shown next to the page; collapsing it is remembered.
    await expect(menu).toBeVisible();
    await expect(menu.getByRole("link", { name: "Explore" })).toHaveAttribute("aria-current", "page");
    await expect(toggle).toHaveAttribute("aria-expanded", "true");
    await tapOrClick(page, toggle);
    await expect(menu).toBeHidden();
    await expect(toggle).toHaveAttribute("aria-expanded", "false");
    await page.reload();
    await expect(page.locator("html")).toHaveAttribute("data-sidebar", "collapsed");
    await expect(menu).toBeHidden();
    await tapOrClick(page, toggle);
    await expect(menu).toBeVisible();
    await page.reload();
    await expect(menu).toBeVisible();
  }
});

test("conf_a11y_04_pages_without_a_context_have_no_side_menu", async ({ page }) => {
  await page.goto("/-/login");
  await expect(page.locator("#app-sidebar")).toHaveCount(0);
  await expect(page.getByRole("button", { name: "Side menu" })).toBeHidden();
});
