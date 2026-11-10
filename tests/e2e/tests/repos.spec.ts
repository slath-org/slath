// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
import { USERS, expect, expectAccessible, sideMenu, signIn, tapOrClick, test } from "../fixtures";

test("conf_a11y_04_create_a_repository_and_open_its_settings", async ({ page }) => {
  await signIn(page, USERS.alice);
  const name = `repo-${test.info().project.name}`.slice(0, 39);
  await page.goto("/-/new");
  await expectAccessible(page);
  await page.getByLabel("Name", { exact: true }).fill(name);
  await tapOrClick(page, page.getByLabel(/^Public\./));
  await tapOrClick(page, page.getByRole("button", { name: "Create repository" }));
  await expect(page.getByRole("heading", { name: "This repository is empty" })).toBeVisible();
  await expectAccessible(page);
  await tapOrClick(page, (await sideMenu(page, "Repository")).getByRole("link", { name: "Settings" }));
  await expect(page.getByRole("heading", { level: 1 })).toHaveText("General");
  await expectAccessible(page);
  for (const section of ["Access", "Branch protection", "Danger zone"]) {
    await tapOrClick(page, page.getByRole("navigation", { name: "Repository settings" }).getByRole("link", { name: section }));
    await expect(page.getByRole("heading", { level: 1 })).toHaveText(section);
    await expectAccessible(page);
  }
});
