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
