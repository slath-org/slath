// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//
// Diff views: the file tree, generated files, files that load later,
// split view on wide screens, and ignoring whitespace. The e2e server seeds
// branch `diffs` with 26 changed files, and `expand` with hidden lines.
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
  await expect(tree.getByRole("link", { name: "f00.txt" })).toBeVisible();
  await expectAccessible(page);

  // The list shows changed content; folded files show, with their
  // reasons, once all files are asked for.
  await expect(page.getByText("2 folded files hidden.")).toBeVisible();
  await expect(tree.getByRole("link", { name: "gen.txt" })).toBeHidden();
  await tapOrClick(page, page.getByLabel("All files (26)"));
  await expect(tree.getByRole("link", { name: "gen.txt" })).toBeVisible();
  await expect(tree.getByRole("listitem").filter({ hasText: "gen.txt" })).toContainText("Added · marked generated in .gitattributes");
  await expect(page.getByText("2 folded files hidden.")).toBeHidden();
  await expectAccessible(page);

  // The generated file is collapsed until asked for.
  const gen = page.locator("section.diff-file").filter({ has: page.getByRole("heading", { name: /gen\.txt/ }) });
  await expect(gen.getByRole("link", { name: "Load diff" })).toBeVisible();
  await expect(gen.locator(".diff-unified")).toHaveCount(0);
  await tapOrClick(page, gen.getByRole("link", { name: "Load diff" }));
  const loaded = page.locator("section.diff-file").filter({ hasText: "generated output" });
  await expect(loaded.locator(".diff-unified")).toBeVisible();

  // Files after the first 20 load when they come into view.
  // (Scrolling the page, since each file is replaced when it loads.)
  const loadedLast = page.locator("section.diff-file").filter({ hasText: "file 22" }).locator(".diff-unified");
  await expect(async () => {
    await page.evaluate(() => window.scrollTo(0, document.body.scrollHeight));
    await expect(loadedLast).toBeVisible({ timeout: 1000 });
  }).toPass();
  // The whitespace-only file is folded with its reason.
  const ws = page.locator("section.diff-file").filter({ has: page.getByRole("heading", { name: /ws\.txt/ }) });
  await expect(ws.getByRole("link", { name: "Load diff" })).toBeVisible();
  await expect(ws.locator(".diff-unified")).toHaveCount(0);
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

test("conf_diff_01_changed_characters_are_marked", async ({ page }) => {
  await signIn(page, USERS.alice);
  await page.goto("/alice/demo/commits/main");
  await tapOrClick(page, page.getByRole("link", { name: "Greet the world" }).first());
  // println!("hello") → println!("hello, world"): only ", world" is marked.
  const ins = page.locator(".diff-unified ins").first();
  await expect(ins).toHaveText("changed: , world");
  await expect(page.locator(".diff-unified del")).toHaveCount(0);
  // Not color alone: underlined, which stays in grayscale and forced colors.
  expect(await ins.evaluate((e) => getComputedStyle(e).textDecorationLine)).toContain("underline");
  await page.emulateMedia({ forcedColors: "active" });
  expect(await ins.evaluate((e) => getComputedStyle(e).textDecorationLine)).toContain("underline");
  await page.emulateMedia({ forcedColors: "none" });
  await expectAccessible(page);
});

test("conf_a11y_04_hunk_navigation_by_link_and_key", async ({ page }) => {
  await signIn(page, USERS.alice);
  await openCommit(page);
  const hunks = page.locator(".diff-hunk");
  const first = hunks.first();
  // The links are the equivalents of the keys.
  const next = first.locator("a", { hasText: "Next change" }).first();
  await expect(next).toHaveAttribute("aria-keyshortcuts", "n");
  // While the diff has focus, n and p move between changes.
  await first.locator(".code-wrap.diff-unified").focus();
  await page.keyboard.press("n");
  await expect(hunks.nth(1).locator(".code-wrap.diff-unified")).toBeFocused();
  await page.keyboard.press("p");
  await expect(first.locator(".code-wrap.diff-unified")).toBeFocused();
  // Outside the diff, n is just a key.
  await page.getByRole("heading", { level: 1 }).click();
  await page.keyboard.press("n");
  await expect(first.locator(".code-wrap.diff-unified")).not.toBeFocused();
  await expectAccessible(page);
});

test("conf_review_17_hidden_lines_expand_in_place", async ({ page }) => {
  await signIn(page, USERS.alice);
  await page.goto("/alice/demo/commits/expand");
  await tapOrClick(page, page.getByRole("link", { name: "Edit long.txt" }).first());
  await expect(page.getByRole("heading", { level: 1 })).toHaveText("Edit long.txt");
  const file = page.locator("#diff-0");
  // Lines 10 and 50 changed: 6 hidden lines above, 33 between, 7 below.
  const links = file.locator(".diff-expand-links a");
  await expect(links).toHaveText(["6 unchanged lines", "33 unchanged lines", "7 unchanged lines"]);
  // Each has the expand icon, and says what it does to assistive technology.
  const icons = await links.locator("use").evaluateAll((us) => us.map((u) => u.getAttribute("href")?.split("#")[1]));
  expect(icons).toEqual(["unfold-vertical", "unfold-vertical", "unfold-vertical"]);
  await expect(links.first()).toHaveAccessibleName("Show 6 unchanged lines");
  await expect(file.getByText("line 30", { exact: true })).toHaveCount(0);
  await expectAccessible(page);
  // In place: the lines between the hunks appear, numbered on both sides,
  // and focus moves to the hunk below them.
  await tapOrClick(page, file.getByRole("link", { name: "Show 33 unchanged lines" }));
  await expect(page.locator("#diff-0").getByText("line 30", { exact: true })).toBeVisible();
  await expect(page).toHaveURL(/\/commit\/[0-9a-f]+$/);
  await expect(page.locator("#diff-0-h1 .code-wrap:visible")).toBeFocused();
  await expect(page.locator("#diff-0 .diff-expand-links a")).toHaveText(["6 unchanged lines", "7 unchanged lines"]);
  // With the keyboard: the next link shows the lines below the last hunk.
  await page.locator("#diff-0").getByRole("link", { name: "Show 7 unchanged lines" }).focus();
  await page.keyboard.press("Enter");
  await expect(page.locator("#diff-0").getByText("line 60", { exact: true })).toBeVisible();
  await expectAccessible(page);
});
