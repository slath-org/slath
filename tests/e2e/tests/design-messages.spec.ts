// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//
// The catalog's messages family: tones, roles, and the busy state.
import { expect, expectAccessible, tapOrClick, test } from "../fixtures";

test("conf_design_03_messages_family_accessible", async ({ page }) => {
  await page.goto("/-/design/messages");
  await expect(page.getByRole("heading", { level: 1 })).toHaveText("Messages and states");
  await expectAccessible(page);
});

test("conf_design_03_messages_roles_and_tones", async ({ page }) => {
  await page.goto("/-/design/messages");
  const notes = page.locator("section[aria-labelledby=notice] [role=note]");
  await expect(notes).toHaveCount(2);
  // The warning has its own tone, not information's.
  const [info, warning] = await notes.evaluateAll((els) => els.map((e) => getComputedStyle(e).backgroundColor));
  expect(warning).not.toBe(info);
  await expect(page.locator("section[aria-labelledby=saved] [role=status]")).toHaveText("Webhook saved.");
  const summary = page.locator("section[aria-labelledby=error-summary] [role=alert]");
  await expect(summary.getByRole("heading", { name: "There is a problem" })).toBeVisible();
  await expect(summary.getByRole("link")).toHaveCount(2);
});

test("conf_design_03_busy_region_shows_a_bar", async ({ page }) => {
  await page.goto("/-/design/messages");
  const bar = await page
    .locator("section[aria-labelledby=busy] .stack[aria-busy=true]")
    .evaluate((el) => {
      const s = getComputedStyle(el, "::before");
      return { content: s.content, height: s.blockSize };
    });
  expect(bar.content).toBe('""');
  expect(bar.height).toBe("2px");
});

test("conf_design_03_toast_from_a_button", async ({ page }) => {
  await page.goto("/-/design/messages");
  await tapOrClick(page, page.getByRole("button", { name: "Show a toast" }));
  await expect(page.locator("#toasts .toast")).toContainText("Copied to the clipboard.");
});
