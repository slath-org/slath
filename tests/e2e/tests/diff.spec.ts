// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//
// Diff views: the file tree, generated files, files that load later,
// split view on wide screens, and ignoring whitespace. The e2e server seeds
// branch `diffs` with 26 changed files.
import { USERS, expect, expectAccessible, signIn, tapOrClick, test } from "../fixtures";

async function openCommit(page: import("@playwright/test").Page): Promise<void> {
  await page.goto("/alice/demo/commits/diffs");
  await tapOrClick(page, page.getByRole("link", { name: "Change many files" }).first());
  await expect(page.getByRole("heading", { level: 1 })).toHaveText("Change many files");
}

test("conf_a11y_01_diff_file_tree_generated_and_later_files", async ({ page }) => {
  test.slow();
  await signIn(page, USERS.alice);
  await openCommit(page);
  await expect(page.getByRole("heading", { name: "26 changed files" })).toBeVisible();
  const tree = page.getByRole("navigation", { name: "Changed files" });
  await expect(tree.getByRole("link", { name: "gen.txt" })).toBeVisible();
  await expectAccessible(page);

  // The generated file is collapsed until asked for.
  const gen = page.locator("section.diff-file").filter({ has: page.getByRole("heading", { name: /gen\.txt/ }) });
  await expect(gen).toContainText("This file is generated");
  await tapOrClick(page, gen.getByRole("link", { name: "Load diff" }));
  const loaded = page.locator("section.diff-file").filter({ hasText: "generated output" });
  await expect(loaded.locator(".diff-unified")).toBeVisible();

  // Files after the first 20 load when they come into view.
  // (Scrolling the page, since each file is replaced when it loads.)
  const loadedLast = page.locator("section.diff-file").filter({ hasText: "alpha" }).locator(".diff-unified");
  await expect(async () => {
    await page.evaluate(() => window.scrollTo(0, document.body.scrollHeight));
    await expect(loadedLast).toBeVisible({ timeout: 1000 });
  }).toPass();
  await expectAccessible(page);
});

test("diff_split_view_only_on_wide_screens_and_ignoring_whitespace", async ({ page }) => {
  await signIn(page, USERS.alice);
  await openCommit(page);
  const url = new URL(page.url());
  await page.goto(`${url.pathname}?view=split&file=24`);
  const wide = (page.viewportSize()?.width ?? 0) >= 1024;
  const section = page.locator("section.diff-file");
  await expect(section).toHaveCount(1);
  if (wide) {
    await expect(section.locator(".diff-split")).toBeVisible();
    await expect(section.locator(".diff-unified")).toBeHidden();
    await expect(page.getByLabel("View")).toBeVisible();
  } else {
    await expect(section.locator(".diff-unified")).toBeVisible();
    await expect(section.locator(".diff-split")).toBeHidden();
    await expect(page.getByLabel("View")).toBeHidden();
  }
  await expectAccessible(page);

  // Whitespace-only changes disappear when ignored.
  await page.goto(`${url.pathname}?w=1`);
  await expect(page.getByRole("heading", { name: "25 changed files" })).toBeVisible();
  await expect(page.getByRole("navigation", { name: "Changed files" }).getByRole("link", { name: "ws.txt" })).toHaveCount(0);
});
