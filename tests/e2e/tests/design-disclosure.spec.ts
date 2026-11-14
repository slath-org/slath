// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//
// The catalog's disclosure family: indicators that turn, and the reply
// box that opens and folds back.
import { type Locator } from "@playwright/test";
import { expect, expectAccessible, tapOrClick, test } from "../fixtures";

async function turn(chevron: Locator): Promise<string> {
  return chevron.evaluate((el) => getComputedStyle(el).transform);
}

test("conf_design_03_disclosure_family_accessible", async ({ page }) => {
  await page.goto("/-/design/disclosure");
  await expect(page.getByRole("heading", { level: 1 })).toHaveText("Disclosure and menus");
  await expectAccessible(page);
});

test("conf_design_03_disclosure_indicator_turns", async ({ page }) => {
  await page.goto("/-/design/disclosure");
  const details = page.locator("details.disclosure").first();
  const summary = details.locator("summary");
  expect(await summary.evaluate((s) => getComputedStyle(s).listStyleType)).toBe("none");
  const chevron = summary.locator(".disclosure-indicator svg");
  const closed = await turn(chevron);
  await tapOrClick(page, summary);
  await expect(details).toHaveAttribute("open", "");
  await expect.poll(() => turn(chevron)).not.toBe(closed);
  await tapOrClick(page, summary);
  await expect(details).not.toHaveAttribute("open", "");
});

test("conf_design_03_menu_chevron_turns", async ({ page }) => {
  await page.goto("/-/design/disclosure");
  const menu = page.locator("section[aria-labelledby=menu] details.menu").first();
  const chevron = menu.locator("summary svg").last();
  const closed = await turn(chevron);
  await tapOrClick(page, menu.locator("summary"));
  await expect.poll(() => turn(chevron)).not.toBe(closed);
  // The icon-only menu has the chevron too.
  await expect(page.locator("section[aria-labelledby=menu] details.menu").nth(1).locator("summary svg")).toHaveCount(2);
});

test("conf_design_03_reply_box_opens_and_folds_back", async ({ page }) => {
  await page.goto("/-/design/disclosure");
  const box = page.locator("details.reply-box");
  const resolve = page.locator(".reply-resolve");
  await expect(resolve).toBeVisible();
  await tapOrClick(page, box.locator("summary"));
  await expect(page.getByLabel("Your reply")).toBeFocused();
  await expect(resolve).toBeHidden();
  await expect(page.getByRole("checkbox", { name: "Resolve thread" })).toBeVisible();
  await expectAccessible(page);
  await tapOrClick(page, page.getByRole("button", { name: "Cancel" }));
  await expect(box).not.toHaveAttribute("open", "");
  await expect(box.locator("summary")).toBeFocused();
  await expect(resolve).toBeVisible();
});
