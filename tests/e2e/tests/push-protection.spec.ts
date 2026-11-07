// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
import { USERS, expect, expectAccessible, signIn, tapOrClick, test } from "../fixtures";

test("conf_a11y_01_secret_allowance_page_accessible", async ({ page }) => {
  await signIn(page, USERS.alice);
  // Each test run uses its own fingerprint, so runs don't see each other's allowances.
  const fingerprint = Array.from({ length: 32 }, () => Math.floor(Math.random() * 16).toString(16)).join("");
  await page.goto(`/alice/demo/-/secret-scan/allow?fingerprint=${fingerprint}&rule=private-key`);
  await expect(page.getByRole("heading", { level: 1 })).toHaveText("Push protection");
  await expectAccessible(page);
  await page.getByLabel("Allow for").selectOption("1");
  await page.getByLabel(/Why it isn't a secret/).fill("Test fixture");
  await tapOrClick(page, page.getByRole("button", { name: "Allow this finding" }));
  await expect(page.getByRole("status").filter({ hasText: "Allowed." })).toBeVisible();
  await expect(page.getByRole("table", { name: "Allowances in force" })).toContainText(fingerprint);
  await expectAccessible(page);
});
