// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//
// Themes, density, and input parity for the Weave components.
import type { Page } from "@playwright/test";
import { USERS, expect, expectAccessible, signIn, tapOrClick, test } from "../fixtures";

const bg = (page: Page) => page.evaluate(() => getComputedStyle(document.body).backgroundColor);

/** Relative luminance (0-1) of an `rgb(...)` string. */
function luminance(rgb: string): number {
  const [r, g, b] = (rgb.match(/[\d.]+/g) ?? []).slice(0, 3).map((v) => {
    const c = Number(v) / 255;
    return c <= 0.04045 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4;
  });
  return 0.2126 * r + 0.7152 * g + 0.0722 * b;
}

test("conf_a11y_03_os_scheme_switch_updates_live", async ({ page }) => {
  await page.emulateMedia({ colorScheme: "light" });
  await page.goto("/-/login");
  const light = await bg(page);
  await page.emulateMedia({ colorScheme: "dark" });
  await expect.poll(() => bg(page)).not.toBe(light);
  expect(luminance(await bg(page))).toBeLessThan(luminance(light));
  await page.emulateMedia({ colorScheme: "light" });
  await expect.poll(() => bg(page)).toBe(light);
});

test("conf_a11y_03_theme_override_persists_without_wrong_first_paint", async ({ page }) => {
  await page.emulateMedia({ colorScheme: "light" });
  await page.goto("/");
  const menu = page.locator("#appearance-menu");
  await tapOrClick(page, menu.locator("summary"));
  const saved = page.waitForResponse((r) => r.url().endsWith("/-/appearance") && r.request().method() === "POST");
  await tapOrClick(page, menu.getByText("Dark", { exact: true }));
  // Applied at once, before the round trip.
  await expect(page.locator("html")).toHaveAttribute("data-theme", "dark");
  await saved;
  // After a reload the server renders the choice into the first byte of
  // HTML, so the first paint already uses the dark tokens.
  const response = await page.goto("/");
  const html = (await response?.text()) ?? "";
  expect(html).toMatch(/<html[^>]* data-theme="dark"/);
  await page.waitForLoadState("domcontentloaded");
  expect(luminance(await bg(page))).toBeLessThan(0.1);
  // The OS scheme no longer matters while the override is set.
  await page.emulateMedia({ colorScheme: "light" });
  expect(luminance(await bg(page))).toBeLessThan(0.1);
  await expect(page.locator('#appearance-menu input[value="dark"]')).toBeChecked();
  // Back to following the system.
  await tapOrClick(page, menu.locator("summary"));
  const back = page.waitForResponse((r) => r.url().endsWith("/-/appearance"));
  await tapOrClick(page, menu.getByText("System", { exact: true }));
  await back;
  const again = await page.goto("/");
  expect((await again?.text()) ?? "").not.toMatch(/<html[^>]* data-theme=/);
});

test("conf_a11y_03_user_setting_follows_the_account", async ({ page, browser }) => {
  // A fresh account per browser cell, created by the site admin.
  const name = `look-${test.info().project.name}`.slice(0, 39);
  await signIn(page, USERS.admin);
  await page.goto("/-/admin/users");
  await page.getByLabel("Username").fill(name);
  await page.getByRole("button", { name: "Create user" }).click();
  const password = await page.locator("#new-password").inputValue();
  const ctx = await browser.newContext({ baseURL: test.info().project.use.baseURL ?? process.env.SLATH_E2E_URL });
  const p2 = await ctx.newPage();
  await signIn(p2, { name, password });
  await p2.goto("/-/settings/appearance");
  await expectAccessible(p2);
  const form = p2.locator("main");
  await form.getByLabel("Light", { exact: true }).check();
  await form.getByLabel(/^Compact/).check();
  await p2.getByLabel("Code font size").selectOption("18");
  await p2.getByRole("button", { name: "Save appearance" }).click();
  await expect(p2.locator("main").getByRole("status")).toContainText("Appearance saved.");
  await ctx.close();
  // Another browser, signed in as the same user: the settings follow.
  const ctx3 = await browser.newContext({ baseURL: process.env.SLATH_E2E_URL });
  const p3 = await ctx3.newPage();
  await p3.emulateMedia({ colorScheme: "dark" });
  await signIn(p3, { name, password });
  const html = p3.locator("html");
  await expect(html).toHaveAttribute("data-theme", "light");
  await expect(html).toHaveAttribute("data-density", "compact");
  await expect(html).toHaveAttribute("data-code-size", "18");
  expect(luminance(await bg(p3))).toBeGreaterThan(0.5);
  await ctx3.close();
});

test("conf_a11y_03_catalog_accessible_in_every_token_set_and_density", async ({ page, context, baseURL }) => {
  // Four axe runs over the whole catalog.
  test.slow();
  for (const density of ["comfortable", "compact"]) {
    await context.addCookies([{ name: "slath_density", value: density, url: baseURL! }]);
    for (const contrast of ["no-preference", "more"] as const) {
      await page.emulateMedia({ contrast });
      await page.goto("/-/design");
      await expect(page.locator("html")).toHaveAttribute("data-density", density);
      await expectAccessible(page);
    }
  }
});

test("conf_a11y_04_components_operable_by_tap_click_and_keyboard", async ({ page }) => {
  await page.goto("/-/design");
  // Menu: pointer opens it, Escape closes it and returns focus.
  const menu = page.locator("#demo-menu");
  await tapOrClick(page, menu.locator("summary"));
  await expect(menu.locator(".menu-panel")).toBeVisible();
  await menu.getByText("Edit").focus();
  await page.keyboard.press("Escape");
  await expect(menu.locator(".menu-panel")).toBeHidden();
  await expect(menu.locator("summary")).toBeFocused();
  // Keyboard opens it too, and a tap outside closes it.
  await page.keyboard.press("Enter");
  await expect(menu.locator(".menu-panel")).toBeVisible();
  await tapOrClick(page, page.getByRole("heading", { name: "Design catalog" }));
  await expect(menu.locator(".menu-panel")).toBeHidden();

  // Dialog and drawer: open by pointer and keyboard, close by Escape and button.
  for (const [opener, id] of [
    ["Open dialog", "#demo-dialog"],
    ["Open drawer", "#demo-drawer"],
  ]) {
    const button = page.getByRole("button", { name: opener });
    const dialog = page.locator(id);
    await tapOrClick(page, button);
    await expect(dialog).toBeVisible();
    await page.keyboard.press("Escape");
    await expect(dialog).toBeHidden();
    await expect(button).toBeFocused();
    await page.keyboard.press("Space");
    await expect(dialog).toBeVisible();
    const close = dialog.getByRole("button", { name: /Cancel|Close/ });
    await close.focus();
    await page.keyboard.press("Enter");
    await expect(dialog).toBeHidden();
  }

  // Toast: shown in the live region, dismissible by pointer.
  await tapOrClick(page, page.getByRole("button", { name: "Show toast" }));
  const toast = page.locator("#toasts .toast");
  await expect(toast).toContainText("Settings saved.");
  await tapOrClick(page, toast.getByRole("button", { name: "Dismiss" }));
  await expect(toast).toHaveCount(0);

  // The appearance menu works from the keyboard alone.
  const appearance = page.locator("#appearance-menu");
  await appearance.locator("summary").focus();
  await page.keyboard.press("Enter");
  await appearance.locator('input[value="dark"]').focus();
  await page.keyboard.press("Space");
  await expect(page.locator("html")).toHaveAttribute("data-theme", "dark");
  await appearance.locator('input[value="system"]').focus();
  await page.keyboard.press("Space");
  await expect(page.locator("html")).not.toHaveAttribute("data-theme", /./);
});

/** Interactive elements smaller than the minimum target size. Inline links
 *  in running text are exempt (WCAG 2.5.8). */
async function smallTargets(page: Page): Promise<string[]> {
  return page.evaluate(() => {
    const coarse = matchMedia("(pointer: coarse)").matches;
    const min = coarse ? 44 : 24;
    const selector =
      "a.button, button, summary, select, input:not([type=hidden]), .menu-item, .tabs a, .sidebar a, .crumbs a, .nav-end a, .site-name, .file-list a";
    const out: string[] = [];
    for (const el of document.querySelectorAll<HTMLElement>(selector)) {
      const style = getComputedStyle(el);
      if (style.visibility === "hidden" || style.display === "none" || el.closest("[hidden], dialog:not([open]), details:not([open]) > :not(summary)")) continue;
      const r = el.getBoundingClientRect();
      if (r.width === 0 && r.height === 0) continue;
      // A small checkbox or radio counts with its label as the target.
      const label = el instanceof HTMLInputElement && el.labels?.[0];
      const box = label ? label.getBoundingClientRect() : r;
      const h = Math.max(r.height, label ? box.height : 0);
      const w = Math.max(r.width, label ? box.width : 0);
      if (h + 0.5 < min || w + 0.5 < min) {
        out.push(`${el.tagName.toLowerCase()}.${el.className} "${(el.textContent ?? "").trim().slice(0, 20)}" ${Math.round(w)}x${Math.round(h)} < ${min}`);
      }
    }
    return out;
  });
}

test("conf_a11y_04_targets_meet_the_minimum_size", async ({ page }) => {
  for (const path of ["/-/design", "/-/login", "/"]) {
    await page.goto(path);
    expect(await smallTargets(page), path).toEqual([]);
    await page.locator("#appearance-menu summary").click();
    expect(await smallTargets(page), `${path} with the appearance menu open`).toEqual([]);
  }
  await signIn(page, USERS.alice);
  for (const path of ["/-/settings/appearance", "/-/settings/profile", "/"]) {
    await page.goto(path);
    expect(await smallTargets(page), path).toEqual([]);
  }
});
