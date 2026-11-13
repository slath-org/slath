// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
import { USERS, expect, expectAccessible, sideMenu, signIn, tapOrClick, test } from "../fixtures";

test("conf_a11y_01_admin_pages_accessible", async ({ page }) => {
  // Seven pages with an axe run each; the audit log grows during the suite.
  test.slow();
  await signIn(page, USERS.admin);
  await tapOrClick(page, page.getByRole("navigation", { name: "Main" }).getByRole("link", { name: "Admin", exact: true }));
  await expect(page.getByRole("heading", { level: 1 })).toHaveText("Users");
  for (const [link, heading] of [
    ["Users", "Users"],
    ["Organizations", "Organizations"],
    ["Bots", "Bots"],
    ["Repositories", "Repositories"],
    ["Audit log", "Audit log"],
    ["Upgrades", "Upgrades"],
    ["Health", "Health"],
  ]) {
    await tapOrClick(page, (await sideMenu(page, "Administration")).getByRole("link", { name: link, exact: true }));
    await expect(page.getByRole("heading", { level: 1 })).toHaveText(heading);
    await expectAccessible(page);
  }
});

test("admin_pages_are_not_found_for_other_users", async ({ page }) => {
  await signIn(page, USERS.alice);
  await expect(page.getByRole("navigation", { name: "Main" }).getByRole("link", { name: "Admin", exact: true })).toHaveCount(0);
  const r = await page.goto("/-/admin/users");
  expect(r?.status()).toBe(404);
});
