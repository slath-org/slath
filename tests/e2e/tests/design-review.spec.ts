// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//
// The catalog's timeline and status family: tones in words, requirement
// rows that open in place, threads that end with the reply box.
import { expect, expectAccessible, tapOrClick, test } from "../fixtures";

test("conf_design_03_review_family_accessible", async ({ page }) => {
  await page.goto("/-/design/review");
  await expect(page.getByRole("heading", { level: 1 })).toHaveText("Timeline and status");
  await expectAccessible(page);
});

test("conf_design_03_status_tones_are_words", async ({ page }) => {
  await page.goto("/-/design/review");
  const checks = page.locator("section[aria-labelledby=status-line] figure li");
  await expect(checks.nth(0)).toContainText("Done:");
  await expect(checks.nth(1)).toContainText("Waiting:");
  await expect(checks.nth(2)).toContainText("Blocking:");
  // Waiting is never red: it shares no color with blocking.
  const colors = await checks.locator(":scope > svg").evaluateAll((s) => s.map((e) => getComputedStyle(e).color));
  expect(colors[1]).not.toBe(colors[2]);
});

test("conf_design_03_requirement_opens_and_thread_ends_with_reply_box", async ({ page }) => {
  await page.goto("/-/design/review");
  const row = page.locator("li.requirement.tone-blocking details");
  await tapOrClick(page, row.locator("summary"));
  await expect(row.getByText("Fix the failure and push again.")).toBeVisible();
  const thread = page.locator("li.timeline-thread");
  await expect(thread.locator("details.reply-box > summary")).toHaveText("Reply…");
  await expect(thread.locator(".reply-resolve")).toBeVisible();
  // Every name in the history starts with its avatar.
  for (const a of await page.locator(".timeline .account").all()) {
    await expect(a.locator(":scope > :first-child")).toHaveClass(/avatar/);
  }
});
