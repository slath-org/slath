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
  for (const name of ["All", "Review requests", "Mine", "Ready to merge"]) {
    await expect(views.getByRole("link", { name, exact: true })).toBeVisible();
  }
  await expect(views.getByRole("link", { name: /^Stalled \(\d+ days?\)$/ })).toBeVisible();
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

test("conf_review_16_a_view_is_saved_and_deleted", async ({ page }, testInfo) => {
  await signIn(page, USERS.alice);
  await page.goto("/alice/demo/merge-requests?state=closed&sort=updated");
  const name = `Closed ${testInfo.project.name}`;
  await tapOrClick(page, page.locator("summary", { hasText: "Save this view" }));
  await page.getByLabel("Name").fill(name);
  await tapOrClick(page, page.getByRole("button", { name: "Save view" }));
  await expect(page).toHaveURL(/state=closed&sort=updated$/);
  await tapOrClick(page, page.locator("summary", { hasText: "View:" }));
  const views = page.getByRole("list", { name: "View" });
  await expect(views.getByRole("link", { name })).toHaveAttribute("aria-current", "page");
  await expectAccessible(page);
  // Another list offers it, and it leads back.
  await page.goto("/alice/demo/merge-requests");
  await tapOrClick(page, page.locator("summary", { hasText: "View:" }));
  await tapOrClick(page, views.getByRole("link", { name }));
  await expect(page).toHaveURL(/state=closed&sort=updated$/);
  await tapOrClick(page, page.getByRole("button", { name: `Delete the saved view ${name}` }));
  await expect(page.locator("summary", { hasText: "Save this view" })).toBeVisible();
});

test("conf_review_16_the_repository_sets_when_merge_requests_are_stalled", async ({ page }) => {
  await signIn(page, USERS.alice);
  await page.goto("/alice/demo/settings/general");
  const days = page.getByLabel("Stalled after (days)");
  await expect(days).toHaveValue(/^\d+$/);
  const before = await days.inputValue();
  await days.fill("0");
  await tapOrClick(page, page.getByRole("button", { name: "Save merge request settings" }));
  await expect(page.getByLabel("Stalled after (days)")).toHaveAttribute("aria-invalid", "true");
  await expect(page.locator("#stalled_after_days-error")).toHaveText("Enter a number of days from 1 to 365.");
  await expectAccessible(page);
  await page.getByLabel("Stalled after (days)").fill(before);
  await tapOrClick(page, page.getByRole("button", { name: "Save merge request settings" }));
  await expect(page.getByText("Merge request settings saved.")).toBeVisible();
  await page.goto("/alice/demo/merge-requests");
  await tapOrClick(page, page.locator("summary", { hasText: "View:" }));
  const label = before === "1" ? "Stalled (1 day)" : `Stalled (${before} days)`;
  await expect(page.getByRole("list", { name: "View" }).getByRole("link", { name: label })).toBeVisible();
});
