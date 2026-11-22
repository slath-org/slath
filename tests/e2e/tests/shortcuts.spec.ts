// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//
// The list of keyboard shortcuts: ? and the footer's button open it, it
// names every shortcut, and its switch turns single-key shortcuts off and
// on (here without an account, so the choice is the browser's cookie).
import { USERS, expect, expectAccessible, signIn, tapOrClick, test } from "../fixtures";

test("conf_design_02_shortcut_list_opens_by_key_and_button", async ({ page }) => {
  await page.goto("/-/login");
  const dialog = page.getByRole("dialog", { name: "Keyboard shortcuts" });
  const button = page.getByRole("button", { name: "Keyboard shortcuts" });
  await expect(button).toBeVisible();
  await expect(button).toHaveAttribute("aria-keyshortcuts", "?");

  // In a field, ? is just a character.
  await page.getByLabel("Username").fill("");
  await page.getByLabel("Username").press("?");
  await expect(dialog).toBeHidden();
  await expect(page.getByLabel("Username")).toHaveValue("?");

  // Outside fields it opens the list; Escape closes it again.
  await page.getByRole("heading", { level: 1 }).click();
  await page.keyboard.press("?");
  await expect(dialog).toBeVisible();
  await expect(dialog.getByRole("heading", { name: "In a diff" })).toBeVisible();
  await expect(dialog.locator("kbd", { hasText: /^n$/ })).toBeVisible();
  await expectAccessible(page);
  await page.keyboard.press("Escape");
  await expect(dialog).toBeHidden();

  // The button opens it too, and focus returns to it on closing.
  await tapOrClick(page, button);
  await expect(dialog).toBeVisible();
  await tapOrClick(page, dialog.getByRole("button", { name: "Close" }));
  await expect(dialog).toBeHidden();
  await expect(button).toBeFocused();

  // Its switch turns single keys off: ? does nothing, the button still works.
  await tapOrClick(page, button);
  await tapOrClick(page, dialog.getByRole("button", { name: "Turn off single-key shortcuts" }));
  await expect(page.locator("html")).toHaveAttribute("data-single-keys", "off");
  await page.getByRole("heading", { level: 1 }).click();
  await page.keyboard.press("?");
  await expect(dialog).toBeHidden();
  await tapOrClick(page, button);
  await expect(dialog).toBeVisible();
  await tapOrClick(page, dialog.getByRole("button", { name: "Turn on single-key shortcuts" }));
  await expect(page.locator("html")).not.toHaveAttribute("data-single-keys", "off");
});

test("conf_design_02_shortcut_list_names_the_editors_shortcuts", async ({ page }) => {
  await signIn(page, USERS.alice);
  await page.goto("/alice/demo/issues/new");
  await tapOrClick(page, page.getByRole("button", { name: "Shortcuts" }).first());
  const help = page.getByRole("dialog", { name: "Editor shortcuts" });
  await expect(help).toBeVisible();
  const keys = await help.locator("td").allTextContents();
  await page.keyboard.press("Escape");
  const listed = await page.locator("#shortcuts kbd").allTextContents();
  // Every key combination of the editor's own help is in the list.
  for (const k of keys.filter((k) => /^(Ctrl|Tab)/.test(k))) expect(listed).toContain(k);
});
