// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//
// Protection rules on facts: the branch protection form offers, folded
// under a disclosure, the facts that need an approval by a person and
// "every changed file viewed", by tap or click and with the keyboard, and
// the rules table says what was chosen in words. What the rules require is tested through the API
// (compat suite). Each browser cell saves a rule for a pattern of its own
// that matches no branch, and removes it again.
import { USERS, expect, expectAccessible, expectConfirmed, signIn, tapOrClick, test } from "../fixtures";

test("conf_agent_14_protection_rules_on_facts_are_chosen_in_the_form", async ({ page }, info) => {
  const pattern = `fact-rules/${info.project.name}`;
  await signIn(page, USERS.alice);
  await page.goto("/alice/demo/settings/protections");
  // The rules on facts start folded away; the disclosure opens them.
  const question = page.getByRole("group", { name: "Needs an approval by a person when it" });
  await expect(question).toBeHidden();
  await tapOrClick(page, page.getByText("Stricter review for some changes"));
  await expect(question.getByRole("checkbox")).toHaveCount(6);
  await expectAccessible(page);
  await page.getByLabel("Branch name or pattern").fill(pattern);
  await tapOrClick(page, question.getByRole("checkbox", { name: "Changes dependencies" }));
  // The keyboard ticks one too.
  await question.getByRole("checkbox", { name: "Deletes tests" }).focus();
  await page.keyboard.press("Space");
  await tapOrClick(page, page.getByRole("checkbox", { name: "Every changed file viewed by an approver" }));
  await tapOrClick(page, page.getByRole("button", { name: "Save rule" }));
  await expectConfirmed(page);
  const row = page.getByRole("row", { name: new RegExp(pattern) });
  await expect(row).toContainText("a person approves when it changes dependencies, deletes tests");
  await expect(row).toContainText("every changed file viewed by an approver");
  await expectAccessible(page);
  await tapOrClick(page, row.getByRole("button", { name: `Remove rule ${pattern}` }));
  await expect(page.getByRole("row", { name: new RegExp(pattern) })).toHaveCount(0);
});
