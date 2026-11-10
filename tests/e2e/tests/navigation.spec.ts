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
  await expectAccessible(page);
  await tapOrClick(page, view);
  const views = page.getByRole("list", { name: "State" });
  await expect(views.getByRole("link", { name: /^Open/ })).toHaveAttribute("aria-current", "page");
  // The open menu covers the list below it: check the menu alone.
  await expectAccessible(page, "details[open]");
  await tapOrClick(page, views.getByRole("link", { name: /^Closed/ }));
  await expect(page).toHaveURL(/state=closed/);
  await expect(page.locator("summary", { hasText: "State:" })).toContainText("Closed");
});

test("ux_nav_01_links_swap_the_page_and_views_swap_the_list", async ({ page }) => {
  await signIn(page, USERS.alice);
  await page.goto("/alice/demo");
  // A marker on window survives only if the page isn't reloaded.
  await page.evaluate(() => ((window as unknown as { kept: boolean }).kept = true));
  const kept = () => page.evaluate(() => (window as unknown as { kept?: boolean }).kept === true);

  // An area of the side menu: the URL, the title, and the menu's current
  // entry change; the page isn't reloaded.
  const menu = page.getByRole("navigation", { name: "Repository", exact: true });
  if (test.info().project.use.hasTouch) await tapOrClick(page, page.getByRole("button", { name: "Side menu" }));
  await tapOrClick(page, menu.getByRole("link", { name: "Issues" }));
  await expect(page).toHaveURL(/\/alice\/demo\/issues$/);
  await expect(page.getByRole("heading", { level: 1 })).toHaveText("Issues");
  await expect(page).toHaveTitle(/^Issues · alice\/demo/);
  // On narrow screens the drawer closes, so look at the markup.
  await expect(page.locator('#app-sidebar a[aria-current="page"]')).toHaveText("Issues");
  if (test.info().project.use.hasTouch) await expect(menu).toBeHidden();
  expect(await kept()).toBe(true);

  // A view of the list swaps only the list: the heading stays the same node.
  await page.locator("h1").evaluate((h) => ((h as unknown as { kept: boolean }).kept = true));
  await tapOrClick(page, page.locator("summary", { hasText: "State:" }));
  await tapOrClick(page, page.getByRole("list", { name: "State" }).getByRole("link", { name: /^Closed/ }));
  await expect(page).toHaveURL(/\/alice\/demo\/issues\?state=closed/);
  await expect(page.locator("summary", { hasText: "State:" })).toContainText("Closed");
  await expect(page.locator("summary", { hasText: "State:" })).toBeFocused();
  expect(await page.locator("h1").evaluate((h) => (h as unknown as { kept?: boolean }).kept === true)).toBe(true);
  expect(await kept()).toBe(true);
  await expectAccessible(page);

  // The URLs are real: reloading gives the same view, and Back returns.
  await page.reload();
  await expect(page.locator("summary", { hasText: "State:" })).toContainText("Closed");
  await page.goBack();
  await expect(page).toHaveURL(/\/alice\/demo\/issues$/);
  await expect(page.locator("summary", { hasText: "State:" })).toContainText("Open");
});

test("conf_a11y_04_alternatives_show_one_at_a_time", async ({ page }) => {
  await signIn(page, USERS.alice);
  await page.goto("/alice/demo");
  await tapOrClick(page, page.locator(".clone-menu > summary"));
  await expect(page.locator("#clone-https")).toBeVisible();
  await expect(page.locator("#clone-ssh")).toBeHidden();
  await tapOrClick(page, page.getByRole("radio", { name: "SSH" }));
  await expect(page.locator("#clone-ssh")).toBeVisible();
  await expect(page.locator("#clone-https")).toBeHidden();
  await expectAccessible(page, ".clone-menu .menu-panel");
});
