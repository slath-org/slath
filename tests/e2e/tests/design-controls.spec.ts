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

test("conf_design_03_popover_takes_focus_and_closes", async ({ page }) => {
  await page.goto("/-/design/controls");
  const summary = page.locator("details.popover > summary");
  await tapOrClick(page, summary);
  const panel = page.getByRole("group", { name: "About push protection" });
  await expect(panel).toBeFocused();
  await expectAccessible(page);
  await page.keyboard.press("Escape");
  await expect(panel).toBeHidden();
  await expect(summary).toBeFocused();
});

test("conf_design_03_password_shows_and_hides", async ({ page }) => {
  await page.goto("/-/design/controls");
  const input = page.locator("#demo-password");
  const toggle = page.getByRole("button", { name: "Show", exact: true });
  await expect(input).toHaveAttribute("type", "password");
  await tapOrClick(page, toggle);
  await expect(input).toHaveAttribute("type", "text");
  await expect(toggle).toHaveAttribute("aria-pressed", "true");
  await tapOrClick(page, toggle);
  await expect(input).toHaveAttribute("type", "password");
});

test("conf_design_03_length_count_near_the_limit", async ({ page }) => {
  await page.goto("/-/design/controls");
  const input = page.locator("#demo-short");
  const count = page.locator("section[aria-labelledby=count] .field-count");
  // 13 of 20 characters: not near the limit yet.
  await expect(count).toBeHidden();
  await input.fill("release-automati");
  await expect(count).toHaveText("4 characters left");
  await expect(input).toHaveAttribute("aria-describedby", /demo-short-hint count-\d+/);
});

test("conf_design_03_names_field_chips", async ({ page }) => {
  await page.goto("/-/design/controls");
  const chips = page.getByRole("list", { name: "Chosen" });
  await expect(chips.getByRole("listitem")).toHaveCount(2);
  await expect(chips.locator(".avatar")).toHaveCount(2);
  const box = page.getByLabel("Reviewers");
  await box.fill("carol,");
  await expect(chips.getByRole("listitem")).toHaveCount(3);
  await tapOrClick(page, chips.getByRole("button", { name: "Remove bob" }));
  await expect(chips.getByRole("listitem")).toHaveCount(2);
  await expect(page.locator("input[type=hidden][name=reviewers]")).toHaveValue("alice, carol");
  await expectAccessible(page);
});
