// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//
// The Context panel: the e2e-agent bot's record of how it made the agent
// fixture's change, on the merge request's page and on the change's page,
// with the start of its transcript and a link to the record's page, by taps
// or clicks and with the keyboard. Who may see a record is tested
// through the API and the page's HTML.
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
  // The panel shows what was asked; the transcript is on the record's page.
  await expect(p).toContainText("Asked: Write notes for the agent fixture.");
  await expect(p).not.toContainText("Done after three tries.");
  await expectAccessible(page);
  // The change it's about has the same record, without the change link.
  await tapOrClick(page, p.getByRole("link", { name: /^[k-z]{8}$/ }));
  await expect(page.getByRole("heading", { name: /^Change / })).toBeVisible();
  await expect(panel(page)).toContainText("Write the agent notes.");
  await expect(panel(page).getByRole("link", { name: /^[k-z]{8}$/ })).toHaveCount(0);
  await expectAccessible(page);
  // The record's page has the whole transcript.
  await tapOrClick(page, panel(page).getByRole("link", { name: "Read the transcript (3 entries)" }));
  await expect(page.getByRole("heading", { level: 1 })).toHaveText("Context record");
  const transcript = page.getByRole("region", { name: "Transcript" });
  await expect(transcript).toContainText("1. Prompt");
  await expect(transcript).toContainText("2. Tool call · editor");
  await expect(transcript).toContainText("Done after three tries.");
  await expect(page.getByRole("link", { name: /^![0-9]+$/ })).toBeVisible();
  await expectAccessible(page);
});

test("conf_agent_05_the_record_page_opens_with_the_keyboard", async ({ page }) => {
  await signIn(page, USERS.alice);
  await openFixture(page);
  await panel(page).getByRole("link", { name: "Read the transcript (3 entries)" }).focus();
  await page.keyboard.press("Enter");
  await expect(page.getByRole("region", { name: "Transcript" })).toContainText("Write notes for the agent fixture.");
});

test("conf_agent_16_the_record_page_offers_deletion_to_who_may_delete", async ({ page }) => {
  // alice owns the agent's bot and administers the repository. The record
  // is shared by every test, so this checks the offer, not the deletion
  // (the compat suite deletes through this form).
  await signIn(page, USERS.alice);
  await openFixture(page);
  await tapOrClick(page, panel(page).getByRole("link", { name: "Read the transcript (3 entries)" }));
  const danger = page.getByRole("region", { name: "Delete this record" });
  await expect(danger).toContainText("backups taken before, still exist");
  await expect(danger.getByRole("button", { name: "Delete record" })).toBeVisible();
  await expectAccessible(page);
});

test("conf_agent_16_agent_context_settings_save_as_they_are", async ({ page }) => {
  await signIn(page, USERS.alice);
  await page.goto("/alice/demo/settings/general");
  const section = page.getByRole("region", { name: "Agent context" });
  const who = section.getByRole("group", { name: "Who sees agent context" });
  await expect(who.getByRole("radio", { name: /^Writers/ })).toBeChecked();
  await expect(section.getByRole("group", { name: "What records keep" }).getByRole("radio", { name: /^Summary and transcript/ })).toBeChecked();
  await expect(section.getByRole("spinbutton", { name: "Delete records after (days)" })).toHaveValue("");
  await expectAccessible(page);
  // Saving the fixture's own values changes nothing other tests rely on.
  await tapOrClick(page, section.getByRole("button", { name: "Save agent context settings" }));
  await expect(page.getByText("Agent context settings saved.")).toBeVisible();
  await expect(page.getByRole("region", { name: "Agent context" }).getByRole("radio", { name: /^Writers/ })).toBeChecked();
});
