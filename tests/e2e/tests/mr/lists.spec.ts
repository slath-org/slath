// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//
// The merge request list (spec: Review lists): open merge requests grouped
// by their readiness headline, in every browser cell. Other scenarios add
// merge requests to the shared server meanwhile, so the checks hold for
// any mix of groups.
import { USERS, expect, expectAccessible, signIn, tapOrClick, test } from "../../fixtures";

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

test("conf_review_16_views_are_picked_from_the_view_menu", async ({ page }) => {
  await signIn(page, USERS.alice);
  await page.goto("/alice/demo/merge-requests");
  const menu = page.locator("summary", { hasText: "View:" });
  await expect(menu).toHaveText(/View: All/);
  await tapOrClick(page, menu);
  const views = page.getByRole("list", { name: "View" });
  for (const name of ["All", "Review requests", "Mine", "Stalled", "Ready to merge"]) {
    await expect(views.getByRole("link", { name, exact: true })).toBeVisible();
  }
  await tapOrClick(page, views.getByRole("link", { name: "Mine", exact: true }));
  await expect(page).toHaveURL(/view=mine/);
  await expect(page.locator("summary", { hasText: "View:" })).toHaveText(/View: Mine/);
  // Every row is Alice's.
  const meta = page.locator(".item-row .item-meta.text-secondary");
  for (const m of await meta.all()) await expect(m.locator(".account").first()).toHaveText(/alice/);
  // The state and order keep the view.
  await tapOrClick(page, page.locator("summary", { hasText: "State:" }));
  await expect(page.getByRole("list", { name: "State" }).getByRole("link", { name: /^Merged/ })).toHaveAttribute("href", /view=mine/);
  await expectAccessible(page);
});
