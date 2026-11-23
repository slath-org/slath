// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//
// The Context panel: the e2e-agent bot's record of how it made the agent
// fixture's change, on the merge request's page and on the change's page,
// with its transcript behind a disclosure, by taps or clicks and with the
// keyboard. Who may see a record is tested through the API.
import type { Page } from "@playwright/test";
import { USERS, expect, expectAccessible, signIn, tapOrClick, test } from "../../fixtures";

const panel = (page: Page) => page.getByRole("region", { name: "Context" });

async function openFixture(page: Page): Promise<void> {
  await page.goto("/alice/demo/merge-requests");
  await tapOrClick(page, page.getByRole("link", { name: "Agent fixture" }));
  await expect(page.getByRole("heading", { level: 1 })).toContainText("Agent fixture");
}

test("conf_agent_05_the_context_panel_shows_an_agents_record", async ({ page }) => {
  await signIn(page, USERS.alice);
  await openFixture(page);
  const p = panel(page);
  await expect(p).toContainText("e2e-agent");
  await expect(p).toContainText("Bot");
  await expect(p).toContainText("Agent's account");
  await expect(p).toContainText("fixture-agent · demo-model · demo-cli");
  await expect(p).toContainText("Write the agent notes.");
  await expect(p).toContainText("Ask alice about the second case");
  await expectAccessible(page);
  // The transcript opens in place.
  const transcript = p.locator("details.disclosure").filter({ hasText: "Transcript, 3 entries" });
  await expect(transcript.locator(".disclosure-indicator")).toBeVisible();
  await tapOrClick(page, transcript.locator("summary"));
  await expect(transcript).toContainText("Write notes for the agent fixture.");
  await expect(transcript).toContainText("Tool call · editor");
  await expectAccessible(page);
  // The change it's about has the same record, without the change link.
  await tapOrClick(page, p.getByRole("link", { name: /^[k-z]{8}$/ }));
  await expect(page.getByRole("heading", { name: /^Change / })).toBeVisible();
  await expect(panel(page)).toContainText("Write the agent notes.");
  await expect(panel(page).getByRole("link", { name: /^[k-z]{8}$/ })).toHaveCount(0);
  await expectAccessible(page);
});

test("conf_agent_05_the_transcript_opens_with_the_keyboard", async ({ page }) => {
  await signIn(page, USERS.alice);
  await openFixture(page);
  const summary = panel(page).locator("details.disclosure summary").filter({ hasText: "Transcript, 3 entries" });
  await summary.focus();
  await page.keyboard.press("Enter");
  await expect(panel(page)).toContainText("Done after three tries.");
});
