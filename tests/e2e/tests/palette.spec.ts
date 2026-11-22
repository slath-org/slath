// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//
// The command palette: Ctrl+K or the global bar's button opens it, it
// offers exactly the page's actions with their keys, and taking one does
// what the page's own link or button does.
import type { Page } from "@playwright/test";
import { USERS, editor, expect, expectAccessible, signIn, tapOrClick, test } from "../fixtures";

const palette = (page: Page) => page.getByRole("dialog", { name: "Actions on this page" });
const options = (page: Page) => palette(page).getByRole("option");

/** The names of the page's links and buttons outside dialogs. */
async function pageNames(page: Page): Promise<string[]> {
  return page.evaluate(() =>
    [...document.querySelectorAll<HTMLElement>("a[href], button")]
      .filter((el) => !el.closest("dialog"))
      .map((el) => (el.getAttribute("aria-label") ?? el.textContent ?? "").replace(/\s+/g, " ").trim()),
  );
}

test("conf_design_02_palette_offers_the_pages_actions", async ({ page }) => {
  await signIn(page, USERS.alice);
  await page.goto("/alice/demo/issues");
  await page.getByRole("heading", { level: 1 }).click();
  await page.keyboard.press("Control+k");
  await expect(palette(page)).toBeVisible();
  await expect(page.getByRole("combobox", { name: "Find an action" })).toBeFocused();
  await expectAccessible(page);

  // The page's places: the global bar, the side menu, the heading, the footer.
  const labels = (await options(page).locator(".palette-label").allTextContents()).map((t) => t.trim());
  for (const name of ["Search", "Notifications", "Issues", "Merge requests", "New issue", "Help", "Keyboard shortcuts"]) {
    expect(labels).toContain(name);
  }
  // Nothing else: every action is a link or button of the page, each once,
  // and neither menu items nor links within the page are listed.
  const names = await pageNames(page);
  for (const l of labels) expect(names).toContain(l);
  const rows = (await options(page).allTextContents()).map((t) => t.trim());
  expect(new Set(rows).size).toBe(rows.length);
  for (const name of ["Skip to main content", "Apply", "Side menu", "Actions on this page"]) {
    expect(labels).not.toContain(name);
  }
  await expect(palette(page).getByRole("status")).toHaveText(`${labels.length} actions`);

  // Typing filters; Enter takes the chosen one.
  await page.keyboard.type("new iss");
  await expect(options(page)).toHaveCount(1);
  await expect(options(page).first()).toHaveAttribute("aria-selected", "true");
  await page.keyboard.press("Enter");
  await expect(page).toHaveURL(/\/alice\/demo\/issues\/new$/);
});

test("conf_design_02_palette_by_button_and_arrows", async ({ page }) => {
  await signIn(page, USERS.alice);
  await page.goto("/alice/demo/issues");
  const button = page.getByRole("navigation", { name: "Main" }).getByRole("button", { name: "Actions on this page" });
  await expect(button).toHaveAttribute("aria-keyshortcuts", "Control+K");
  await tapOrClick(page, button);
  await expect(palette(page)).toBeVisible();
  // Up from the first wraps to the last; Escape closes and focus returns.
  const input = page.getByRole("combobox", { name: "Find an action" });
  await input.press("ArrowUp");
  await expect(options(page).last()).toHaveAttribute("aria-selected", "true");
  await expect(input).toHaveAttribute("aria-activedescendant", (await options(page).last().getAttribute("id")) ?? "");
  await input.press("Escape");
  await expect(palette(page)).toBeHidden();
  await expect(button).toBeFocused();
  // A tap or click takes an action.
  await tapOrClick(page, button);
  await tapOrClick(page, options(page).filter({ hasText: "Keyboard shortcuts" }));
  await expect(page.getByRole("dialog", { name: "Keyboard shortcuts" })).toBeVisible();
});

test("conf_design_02_palette_names_shortcuts_and_leaves_the_editor_its_keys", async ({ page }) => {
  await signIn(page, USERS.alice);
  await page.goto("/alice/demo/commits/diffs");
  await tapOrClick(page, page.getByRole("link", { name: "Change many files" }).first());
  await expect(page.getByRole("heading", { level: 1 })).toHaveText("Change many files");
  await page.getByRole("heading", { level: 1 }).click();
  await page.keyboard.press("Control+k");
  // Controls with shortcuts are actions, with their keys, once even where
  // every hunk has one.
  const next = options(page).filter({ hasText: "Next change" });
  await expect(next).toHaveCount(1);
  await expect(next.locator("kbd")).toHaveText("n");
  await page.keyboard.press("Escape");

  // In the editor, Ctrl+K stays its link shortcut.
  await page.goto("/alice/demo/issues/new");
  await expect(page.locator("slath-editor[data-state=ready]")).toBeVisible();
  await tapOrClick(page, editor(page, "Description"));
  await page.keyboard.press("Control+k");
  await expect(palette(page)).toBeHidden();
});
