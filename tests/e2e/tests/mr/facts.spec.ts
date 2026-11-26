// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//
// The Facts panel: what Slath sees in the facts fixture's diff, on the
// merge request's page and on its change's page, with only the facts that
// apply and the rest of a long list folded, opened by tap, click, or the
// keyboard. Exactness is tested through the API (compat suite).
import type { Page } from "@playwright/test";
import { USERS, expect, expectAccessible, signIn, tapOrClick, test } from "../../fixtures";

const panel = (page: Page) => page.getByRole("region", { name: "Facts" });

test("conf_agent_14_the_facts_panel_lists_what_applies", async ({ page }) => {
  await signIn(page, USERS.alice);
  await page.goto("/alice/demo/merge-requests");
  await tapOrClick(page, page.getByRole("link", { name: "Facts fixture" }));
  const p = panel(page);
  await expect(p).toContainText("2 files, +23 −0");
  await expect(p).toContainText("Observed");
  await expect(p.getByRole("term")).toHaveText(["Dependencies", "Mode changes"]);
  await expect(p).toContainText("Cargo.lock: added alpha 1.0.0");
  await expect(p).toContainText("run.sh: new executable file");
  // Three packages show; the other two are folded.
  await expect(p.getByText("added delta 1.0.0")).toBeHidden();
  await tapOrClick(page, p.getByText("2 more"));
  await expect(p.getByText("added delta 1.0.0")).toBeVisible();
  await expectAccessible(page);
  // The change's page has the facts of the change's own diff.
  await tapOrClick(page, page.getByRole("navigation", { name: "Merge request" }).getByRole("link", { name: "Changes" }));
  await tapOrClick(page, page.locator("section[aria-labelledby=changes-heading] .item-list > li a.item-title").first());
  await expect(panel(page)).toContainText("Cargo.lock: added alpha 1.0.0");
  await expectAccessible(page);
});

test("conf_agent_14_the_facts_fold_opens_with_the_keyboard", async ({ page }) => {
  await signIn(page, USERS.alice);
  await page.goto("/alice/demo/merge-requests");
  await tapOrClick(page, page.getByRole("link", { name: "Facts fixture" }));
  const more = panel(page).getByText("2 more");
  await more.focus();
  await page.keyboard.press("Enter");
  await expect(panel(page).getByText("added echo 1.0.0")).toBeVisible();
});
