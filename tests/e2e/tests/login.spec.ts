// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
import { USERS, expect, expectAccessible, signIn, tapOrClick, test } from "../fixtures";

test("conf_a11y_01_login_page_accessible", async ({ page }) => {
  await page.goto("/-/login");
  await expect(page).toHaveTitle("Sign in · Slath");
  await expectAccessible(page);
});

test("conf_a11y_01_failed_login_shows_an_accessible_error_summary", async ({ page }) => {
  // A per-project unknown account: real accounts would get throttled after
  // ten failures across the parallel projects (unknown and wrong look alike).
  const name = `nobody-${test.info().project.name}`;
  await page.goto("/-/login");
  await page.getByLabel("Username").fill(name);
  await page.getByLabel("Password").fill("not the password");
  await tapOrClick(page, page.getByRole("button", { name: "Sign in" }));
  const summary = page.getByRole("alert");
  await expect(summary).toContainText("Incorrect username or password.");
  await expect(page.getByLabel("Username")).toHaveValue(name);
  await expect(page.getByLabel("Password")).toHaveAttribute("aria-invalid", "true");
  await expectAccessible(page);
});

test("conf_a11y_04_sign_in_and_out_by_touch_or_click", async ({ page }) => {
  await signIn(page, USERS.alice);
  await expectAccessible(page);
  await tapOrClick(page, page.getByRole("button", { name: "Sign out" }));
  await expect(page.getByRole("link", { name: "Sign in" })).toBeVisible();
});

test("conf_a11y_04_sign_in_by_keyboard", async ({ page }) => {
  await page.goto("/-/login");
  await page.getByLabel("Username").focus();
  await page.keyboard.type(USERS.alice.name);
  await page.keyboard.press("Tab");
  await page.keyboard.type(USERS.alice.password);
  await page.keyboard.press("Enter");
  await expect(page.getByRole("navigation", { name: "Main" }).getByRole("link", { name: "alice", exact: true })).toBeVisible();
});
