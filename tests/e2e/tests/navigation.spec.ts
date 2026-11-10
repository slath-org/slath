// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//
// The navigation levels: the side menu of a context, tabs for the sections
// of an area, and a menu for the views of a list.
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

test("conf_a11y_04_sections_of_an_area_are_tabs", async ({ page }) => {
  await signIn(page, USERS.alice);
  await page.goto("/alice/demo/commits");
  const code = page.getByRole("navigation", { name: "Code" });
  await expect(code.getByRole("link", { name: "Commits" })).toHaveAttribute("aria-current", "page");
  await tapOrClick(page, code.getByRole("link", { name: "Branches" }));
  await expect(page.getByRole("heading", { level: 1 })).toHaveText("Branches");
});

test("conf_a11y_04_views_of_a_list_are_a_menu", async ({ page }) => {
  await signIn(page, USERS.alice);
  await page.goto("/alice/demo/issues");
  const view = page.locator("summary", { hasText: "State:" });
  await expect(view).toContainText("Open");
  await tapOrClick(page, view);
  const views = page.getByRole("list", { name: "State" });
  await expect(views.getByRole("link", { name: /^Open/ })).toHaveAttribute("aria-current", "page");
  await expectAccessible(page);
  await tapOrClick(page, views.getByRole("link", { name: /^Closed/ }));
  await expect(page).toHaveURL(/state=closed/);
  await expect(page.locator("summary", { hasText: "State:" })).toContainText("Closed");
});
