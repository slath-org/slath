// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
import { USERS, expect, expectAccessible, signIn, tapOrClick, test } from "../fixtures";

test("conf_a11y_04_create_an_organization_and_manage_members", async ({ page }) => {
  await signIn(page, USERS.alice);
  const org = `org-${test.info().project.name}`.slice(0, 39);
  await page.goto("/-/orgs/new");
  await expectAccessible(page);
  await page.getByLabel("Name", { exact: true }).fill(org);
  await tapOrClick(page, page.getByRole("button", { name: "Create organization" }));
  await expect(page.getByRole("heading", { level: 1 })).toHaveText(org);
  await expect(page.getByRole("status").filter({ hasText: "Organization created." })).toBeVisible();
  await expectAccessible(page);
  await tapOrClick(page, page.getByRole("link", { name: "Members" }));
  await expect(page.getByRole("heading", { level: 1 })).toHaveText("Members");
  await expectAccessible(page);

  // Teams and the member policies.
  await page.goto(`/${org}`);
  await tapOrClick(page, page.getByRole("link", { name: "Teams" }));
  await expect(page.getByRole("heading", { level: 1 })).toHaveText("Teams");
  await expectAccessible(page);
  await page.getByLabel("Name", { exact: true }).fill("devs");
  await tapOrClick(page, page.getByRole("button", { name: "Create team" }));
  await expect(page.getByRole("status").filter({ hasText: "Team created." })).toBeVisible();
  await tapOrClick(page, page.getByRole("link", { name: "devs", exact: true }));
  await expect(page.getByRole("heading", { level: 1 })).toHaveText("devs");
  await expectAccessible(page);
  await page.goto(`/${org}/-/settings`);
  await expect(page.getByRole("heading", { name: "Member policies" })).toBeVisible();
  await expectAccessible(page);
});

test("conf_a11y_01_user_profile_accessible", async ({ page }) => {
  await page.goto(`/${USERS.alice.name}`);
  await expect(page.getByRole("heading", { level: 1 })).toContainText("alice");
  await expectAccessible(page);
});
