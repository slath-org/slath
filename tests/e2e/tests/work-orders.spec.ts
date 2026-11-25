// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//
// Delegating an issue to a bot from its page, the order's page, and
// stopping the order, by keyboard-free taps or clicks in every cell, with
// axe on each page. The e2e server seeds `e2e-agent`, a bot with write
// access to alice/demo.
import { expect, expectAccessible, signIn, tapOrClick, test, USERS } from "../fixtures";

test("conf_agent_13_an_issue_is_delegated_to_a_bot_from_its_page", async ({ page }, testInfo) => {
  await signIn(page, USERS.alice);
  await page.goto("/alice/demo/issues/new");
  const title = `Delegate me in ${testInfo.project.name}`;
  await page.getByLabel("Title").fill(title);
  await tapOrClick(page, page.getByRole("button", { name: "Open issue" }));
  await expect(page.getByRole("heading", { level: 1 })).toContainText(title);

  const side = page.getByRole("region", { name: "Work order" });
  await tapOrClick(page, side.getByRole("link", { name: "Delegate to a bot" }));
  await expect(page.getByRole("heading", { level: 1 })).toHaveText(/^Delegate issue #\d+ to a bot$/);
  await expect(page.getByLabel("Bot", { exact: true })).toHaveValue("e2e-agent");
  await expect(page.getByLabel("Goal")).toHaveValue(title);
  await expectAccessible(page);

  // A mistake in the limits opens them and says what's wrong.
  await tapOrClick(page, page.getByText("Limits and review"));
  await page.getByLabel(/^Revisions/).fill("0");
  await tapOrClick(page, page.getByRole("button", { name: "Delegate" }));
  await expect(page.getByRole("alert")).toContainText("Allow between 1 and");
  await expect(page.getByLabel(/^Revisions/)).toBeVisible();
  await expectAccessible(page);
  await page.getByLabel(/^Revisions/).fill("5");
  await page.getByLabel(/^Required checks/).fill("ci/test");
  await tapOrClick(page, page.getByRole("button", { name: "Delegate" }));

  // The order's page.
  await expect(page.getByRole("heading", { level: 1 })).toHaveText(/^Work order \d+ Open$/);
  await expect(page.getByText("0 of 5")).toBeVisible();
  await expect(page.getByRole("region", { name: "Goal" })).toContainText(title);
  await expect(page.getByRole("region", { name: "Events" })).toContainText("work_order.create");
  await expectAccessible(page);

  // Stopping it.
  await tapOrClick(page, page.getByRole("button", { name: "Stop order" }));
  await expect(page.getByRole("heading", { level: 1 })).toHaveText(/^Work order \d+ Stopped$/);
  await expect(page.getByRole("button", { name: "Stop order" })).toHaveCount(0);
  await expectAccessible(page);

  // The issue names its order and offers to delegate again.
  await tapOrClick(page, page.getByRole("link", { name: /^issue #\d+:/ }));
  await expect(side.getByRole("link", { name: /^Work order \d+$/ })).toBeVisible();
  await expect(side).toContainText("Stopped");
  await expect(side.getByRole("link", { name: "Delegate to a bot" })).toBeVisible();
  await expectAccessible(page);
});

test("conf_agent_19_delegating_an_outsiders_merge_request_asks_first", async ({ page }) => {
  // alice/contrib's "Outside fixture" comes from e2e-outsider's fork.
  await signIn(page, USERS.alice);
  await page.goto("/alice/contrib/merge-requests");
  await tapOrClick(page, page.getByRole("link", { name: "Outside fixture" }));
  const side = page.getByRole("region", { name: "Work order" });
  await tapOrClick(page, side.getByRole("link", { name: "Delegate to a bot" }));
  await expect(page.getByRole("heading", { level: 1 })).toHaveText(/^Delegate merge request !\d+ to a bot$/);
  const warning = page.getByRole("note").filter({ hasText: "an outside contributor" });
  await expect(warning).toBeVisible();
  await expect(page.getByRole("radio", { name: /^Members/ })).toHaveCount(0);
  await expectAccessible(page);

  // Without the box, the form says what's missing.
  await tapOrClick(page, page.getByRole("button", { name: "Delegate" }));
  await expect(page.getByText("Check the box to delegate it.")).toBeVisible();
  await expectAccessible(page);
  // Checking it is one tap; delegating is left to the API tests, since
  // every project shares this merge request.
  await tapOrClick(page, page.getByLabel(/^Let the bot read all content/));
  await expect(page.getByLabel(/^Let the bot read all content/)).toBeChecked();
});
