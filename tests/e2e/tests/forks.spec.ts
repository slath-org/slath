// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//
// Forking from the repository header into an organization: the form (a
// private repository's fork can only be private, and its own namespace isn't
// offered), the fork's "forked from" and sync lines once its copy is done,
// and the parent's list of forks.
import { USERS, expect, expectAccessible, signIn, tapOrClick, test } from "../fixtures";

test("conf_a11y_04_fork_a_repository", async ({ page }) => {
  await signIn(page, USERS.alice);
  // alice/demo is in alice's own namespace: she can fork it only into an
  // organization of hers.
  const org = `forks-${test.info().project.name}`.slice(0, 39);
  await page.goto("/-/orgs/new");
  await page.getByLabel("Name", { exact: true }).fill(org);
  await tapOrClick(page, page.getByRole("button", { name: "Create organization" }));
  await expect(page.getByRole("heading", { level: 1 })).toHaveText(org);
  const fork = `${org}/demo`;
  await page.goto("/alice/demo");
  await tapOrClick(page, page.getByRole("link", { name: "Fork", exact: true }));
  // Links swap the page without a reload, so wait for the new address.
  await expect(page).toHaveURL(/\/alice\/demo\/fork$/);
  await expect(page.getByRole("heading", { level: 1 })).toHaveText("Fork this repository");
  await expect(page.getByLabel("Owner").locator("option", { hasText: /^alice$/ })).toHaveCount(0);
  await expect(page.getByLabel(/^Public\./)).toHaveCount(0);
  await expect(page.getByText("A fork can't be more visible")).toBeVisible();
  await expectAccessible(page);
  await page.getByLabel("Owner").selectOption(org);
  await tapOrClick(page, page.getByRole("button", { name: "Create fork" }));
  await expect(page).toHaveURL(new RegExp(`/${fork}$`));
  await expect(page.getByText("Forked from")).toContainText("alice/demo");
  // The copy runs in the background; once done, the code is there.
  await expect(async () => {
    await page.reload();
    await expect(page.getByText("This fork is being created")).toHaveCount(0, { timeout: 500 });
    await expect(page.getByText("README.md").first()).toBeVisible({ timeout: 500 });
  }).toPass();
  await expect(page.getByText("is up to date with alice/demo")).toBeVisible();
  await expectAccessible(page);
  await page.goto(`/${fork}/sync`);
  await expect(page.getByRole("heading", { level: 1 })).toHaveText("Sync fork");
  await expect(page.locator("main")).toContainText("is up to date with alice/demo");
  await expectAccessible(page);

  await page.goto("/alice/demo/forks");
  await expect(page.getByRole("heading", { level: 1 })).toHaveText("Forks");
  await expect(page.getByRole("link", { name: fork })).toBeVisible();
  await expectAccessible(page);
});
