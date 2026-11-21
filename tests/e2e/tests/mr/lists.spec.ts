// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//
// The merge request list (spec: Review lists): open merge requests grouped
// by their readiness headline, in every browser cell. Other scenarios add
// merge requests to the shared server meanwhile, so the checks hold for
// any mix of groups.
import { USERS, expect, expectAccessible, signIn, test } from "../../fixtures";

test("conf_review_16_open_list_is_grouped_by_readiness", async ({ page }) => {
  await signIn(page, USERS.alice);
  await page.goto("/alice/demo/merge-requests");
  const groups = page.locator("section.item-group");
  await expect(groups.first()).toBeVisible();
  // Each group is a section named by its heading: the tone in words, the
  // headline, and its count, never color alone.
  const headings = groups.getByRole("heading", { level: 2 });
  const n = await headings.count();
  expect(n).toBeGreaterThan(0);
  for (let i = 0; i < n; i++) {
    await expect(headings.nth(i)).toHaveText(/^((Done|Waiting|Blocking): )?.+ \d+ merge requests?$/);
  }
  await expect(page.getByRole("region").first()).toBeVisible();
  // Ready to merge comes first, when there is such a group.
  const names = await headings.allInnerTexts();
  const ready = names.findIndex((t) => t.includes("Ready to merge"));
  if (ready >= 0) expect(ready).toBe(0);
  // The rows don't repeat the headline as an icon.
  await expect(page.locator(".item-group .item-checks")).toHaveCount(0);
  await expectAccessible(page);
});

test("conf_review_16_other_lists_keep_the_readiness_icon", async ({ page }) => {
  await signIn(page, USERS.alice);
  await page.goto("/alice/demo/merge-requests?state=all");
  await expect(page.locator("section.item-group")).toHaveCount(0);
  await expect(page.locator(".item-checks").first()).toBeVisible();
  await expectAccessible(page);
});
