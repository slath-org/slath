// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
import { expect, expectAccessible, expectConfirmed, sideMenu, signIn, tapOrClick, test, USERS } from "../fixtures";

test("conf_a11y_04_create_a_repository_and_open_its_settings", async ({ page }) => {
  await signIn(page, USERS.alice);
  const name = `repo-${test.info().project.name}`.slice(0, 39);
  await page.goto("/-/new");
  await expectAccessible(page);
  await page.getByLabel("Name", { exact: true }).fill(name);
  await tapOrClick(page, page.getByRole("radio", { name: /^Public/ }));
  await tapOrClick(page, page.getByRole("button", { name: "Create repository" }));
  await expect(page.getByRole("heading", { name: "This repository is empty" })).toBeVisible();
  await expectAccessible(page);
  await tapOrClick(page, (await sideMenu(page, "Repository")).getByRole("link", { name: "Settings" }));
  await expect(page.getByRole("heading", { level: 1 })).toHaveText("General");
  await expectAccessible(page);
  for (const section of ["Access", "Deploy keys", "Branch protection", "Danger zone"]) {
    await tapOrClick(page, page.getByRole("navigation", { name: "Repository settings" }).getByRole("link", { name: section }));
    await expect(page.getByRole("heading", { level: 1 })).toHaveText(section);
    await expectAccessible(page);
  }
  // Sensitive paths: the built-in ones fold open, the repository's own save.
  await tapOrClick(page, page.getByRole("navigation", { name: "Repository settings" }).getByRole("link", { name: "Branch protection" }));
  await tapOrClick(page, page.getByText(/^Built-in paths/));
  await expect(page.getByText("**/AGENTS.md", { exact: true })).toBeVisible();
  await page.getByLabel("This repository's own paths").fill("deploy/**");
  await tapOrClick(page, page.getByRole("button", { name: "Save sensitive paths" }));
  await expectConfirmed(page);
  await expect(page.getByLabel("This repository's own paths")).toHaveValue("deploy/**");
  await expectAccessible(page);
});
