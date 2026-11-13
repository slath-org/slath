// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
import { USERS, expect, expectAccessible, sideMenu, signIn, tapOrClick, test } from "../fixtures";

test("conf_a11y_01_create_a_bot_and_give_it_a_token", async ({ page }) => {
  await signIn(page, USERS.alice);
  const bot = `ci-${test.info().project.name}`.replace(/[^a-z0-9-]/gi, "-").slice(0, 39).toLowerCase();
  await page.goto("/-/settings/profile");
  await tapOrClick(page, (await sideMenu(page, "Settings")).getByRole("link", { name: "Bots" }));
  await expect(page.getByRole("heading", { level: 1 })).toHaveText("Bots");
  await expectAccessible(page);

  // A taken name is reported at the field.
  await page.getByLabel("Username").fill("alice");
  await tapOrClick(page, page.getByRole("button", { name: "Create bot" }));
  await expect(page.getByLabel("Username")).toHaveAttribute("aria-invalid", "true");
  await expectAccessible(page);

  await page.getByLabel("Username").fill(bot);
  await tapOrClick(page, page.getByRole("button", { name: "Create bot" }));
  await expect(page.getByRole("heading", { level: 1 })).toContainText(bot);
  await expect(page.getByRole("status").filter({ hasText: "Bot created." })).toBeVisible();
  await expectAccessible(page);

  // Its profile can be changed on its page.
  await page.getByLabel("Display name").fill("Release automation");
  await tapOrClick(page, page.getByRole("button", { name: "Save profile" }));
  await expect(page.getByRole("status").filter({ hasText: "Profile saved." })).toBeVisible();
  await expect(page.getByLabel("Display name")).toHaveValue("Release automation");
  await expectAccessible(page);

  // A token without repositories is refused; with one, it's shown once.
  await page.getByLabel("Name", { exact: true }).fill("release job");
  await tapOrClick(page, page.getByRole("button", { name: "Create token" }));
  await expect(page.getByLabel("Repositories", { exact: true })).toHaveAttribute("aria-invalid", "true");
  await expectAccessible(page);
  await page.getByLabel("Repositories", { exact: true }).fill(`${USERS.alice.name}/demo`);
  await tapOrClick(page, page.getByRole("button", { name: "Create token" }));
  await expect(page.getByLabel("New token")).toHaveValue(/^slath_bot_/);
  await expect(page.getByRole("table", { name: `Tokens of ${bot}` })).toContainText("release job");
  await expectAccessible(page);
});

test("conf_agent_01_stop_and_re_enable_a_bot", async ({ page }) => {
  await signIn(page, USERS.alice);
  const bot = `stop-${test.info().project.name}`.replace(/[^a-z0-9-]/gi, "-").slice(0, 39).toLowerCase();
  await page.goto("/-/settings/bots");
  await page.getByLabel("Username").fill(bot);
  await tapOrClick(page, page.getByRole("button", { name: "Create bot" }));
  await expect(page.getByRole("heading", { level: 1 })).toContainText(bot);
  await expect(page.getByRole("term").filter({ hasText: "API requests this hour" })).toBeVisible();
  await expectAccessible(page);

  await tapOrClick(page, page.getByRole("button", { name: "Stop bot" }));
  await expect(page.getByRole("status").filter({ hasText: "Bot stopped." })).toBeVisible();
  await expect(page.getByRole("heading", { level: 1 })).toContainText("Stopped");
  await expectAccessible(page);
  await tapOrClick(page, page.getByRole("button", { name: "Re-enable bot" }));
  await expect(page.getByRole("status").filter({ hasText: "Bot re-enabled." })).toBeVisible();
  await expect(page.getByRole("button", { name: "Stop bot" })).toBeVisible();

  // The list shows each bot's state and activity.
  await page.goto("/-/settings/bots");
  const row = page.getByRole("row").filter({ hasText: bot });
  await expect(row).toContainText("Active");
  await expect(row).toContainText("No active token");
  await expectAccessible(page);
});

test("conf_agent_10_freeze_an_organizations_bots", async ({ page }) => {
  await signIn(page, USERS.alice);
  const org = `fz-${test.info().project.name}`.replace(/[^a-z0-9-]/gi, "-").slice(0, 39).toLowerCase();
  await page.goto("/-/orgs/new");
  await page.getByLabel("Name", { exact: true }).fill(org);
  await tapOrClick(page, page.getByRole("button", { name: "Create organization" }));
  await expect(page.getByRole("heading", { level: 1 })).toHaveText(org);
  await page.goto(`/${org}/-/bots`);
  await expect(page.getByRole("heading", { name: `Freeze the bots of ${org}` })).toBeVisible();
  await expectAccessible(page);
  await tapOrClick(page, page.getByRole("button", { name: "Freeze bots" }));
  await expect(page.getByRole("status").filter({ hasText: "Bots frozen." })).toBeVisible();
  await expect(page.getByText(`The bots of ${org} are frozen`)).toBeVisible();
  await expectAccessible(page);
  await tapOrClick(page, page.getByRole("button", { name: "Unfreeze bots" }));
  await expect(page.getByRole("status").filter({ hasText: "Bots unfrozen." })).toBeVisible();
});

test("conf_agent_01_administrators_see_every_bot", async ({ page }) => {
  await signIn(page, USERS.admin);
  await page.goto("/-/admin/bots");
  await expect(page.getByRole("heading", { level: 1 })).toHaveText("Bots");
  await expect(page.getByRole("row").filter({ hasText: "e2e-lint" })).toContainText("alice");
  await expect(page.getByRole("heading", { name: "Freeze all bots" })).toBeVisible();
  await expectAccessible(page);
});
