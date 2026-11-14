// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//
// The catalog's basic controls: tooltips that work with a mouse, the
// keyboard, and touch; times, hashes, paths, and cut text.
import { expect, expectAccessible, tapOrClick, test } from "../fixtures";

test("conf_design_03_controls_family_accessible", async ({ page }) => {
  await page.goto("/-/design/controls");
  await expect(page.getByRole("heading", { level: 1 })).toHaveText("Basic controls");
  await expectAccessible(page);
  // No browser tooltips: they never show on touch or with the keyboard.
  await expect(page.locator("main [title]")).toHaveCount(0);
});

test("conf_design_03_tooltip_on_focus_and_tap", async ({ page }) => {
  await page.goto("/-/design/controls");
  const tip = page.getByRole("tooltip");
  const button = page.getByRole("button", { name: "Edit comment" });
  if (test.info().project.use.hasTouch) {
    await button.tap();
  } else {
    await button.focus();
  }
  await expect(tip).toHaveText("Edit comment");
  await expect(tip).toBeInViewport();
  await page.keyboard.press("Escape");
  await expect(tip).toBeHidden();
});

test("conf_design_03_time_shows_short_form_and_exact_tip", async ({ page }) => {
  await page.goto("/-/design/controls");
  const times = page.locator("section[aria-labelledby=time] time");
  await expect(times.nth(0)).toHaveText("3 hours ago");
  await expect(times.nth(1)).toHaveText("in 2 days");
  await expect(times.nth(2)).toHaveText(/^\d{1,2} [A-Z][a-z]{2} \d{4}$/);
  await expect(times.nth(3)).toHaveText(/^\d{4}-\d{2}-\d{2} \d{2}:\d{2} UTC$/);
  await expect(times.nth(0)).toHaveAttribute("data-tip", /UTC$/);
  await tapOrClick(page, times.nth(0));
  if (test.info().project.use.hasTouch) {
    await expect(page.getByRole("tooltip")).toHaveText(/UTC$/);
  }
});

test("conf_design_03_hash_path_cut", async ({ page }) => {
  await page.goto("/-/design/controls");
  const hash = page.locator(".hash");
  await expect(hash).toHaveText("a1e62f5c7d0b");
  await expect(hash).toHaveAttribute("data-tip", "a1e62f5c7d0b9a8e3f41c2d6b7e8f90a1b2c3d4e");
  await expect(page.locator(".path wbr")).toHaveCount(4);
  const cut = await page.locator(".cut").evaluate((el) => el.scrollWidth > el.clientWidth);
  expect(cut).toBe(true);
});
