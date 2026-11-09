// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//
// Forking from the repository header: the form (a private repository's
// fork can only be private), the fork's "forked from" and sync lines once
// its copy is done, and the parent's list of forks.
import { USERS, expect, expectAccessible, signIn, tapOrClick, test } from "../fixtures";

test("conf_a11y_04_fork_a_repository", async ({ page }) => {
  await signIn(page, USERS.alice);
  const name = `fork-${test.info().project.name}`.slice(0, 39);
  await page.goto("/alice/demo");
  await tapOrClick(page, page.getByRole("link", { name: "Fork", exact: true }));
  await expect(page.getByRole("heading", { level: 1 })).toHaveText("Fork this repository");
  await expect(page.getByLabel(/^Public\./)).toHaveCount(0);
  await expect(page.getByText("A fork can't be more visible")).toBeVisible();
  await expectAccessible(page);
  await page.getByLabel("Name", { exact: true }).fill(name);
  await tapOrClick(page, page.getByRole("button", { name: "Create fork" }));
  await expect(page).toHaveURL(new RegExp(`/alice/${name}$`));
  await expect(page.getByText("Forked from")).toContainText("alice/demo");
  // The copy runs in the background; once done, the code is there.
  await expect(async () => {
    await page.reload();
    await expect(page.getByText("This fork is being created")).toHaveCount(0, { timeout: 500 });
    await expect(page.getByText("README.md").first()).toBeVisible({ timeout: 500 });
  }).toPass();
  await expect(page.getByText("is up to date with alice/demo")).toBeVisible();
  await expectAccessible(page);
  await page.goto(`/alice/${name}/sync`);
  await expect(page.getByRole("heading", { level: 1 })).toHaveText("Sync fork");
  await expect(page.locator("main")).toContainText("is up to date with alice/demo");
  await expectAccessible(page);

  await page.goto("/alice/demo/forks");
  await expect(page.getByRole("heading", { level: 1 })).toHaveText("Forks");
  await expect(page.getByRole("link", { name: `alice/${name}` })).toBeVisible();
  await expectAccessible(page);
});
