// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//
// Changing a branch protection rule: "Change" opens the rule in the form
// with everything it holds, so changing one setting doesn't mean entering
// the others again, and a save that fails keeps what was entered. Each
// browser cell uses a pattern of its own that matches no branch, and
// removes a rule it saved again.
import { USERS, expect, expectAccessible, expectConfirmed, expectProblem, signIn, tapOrClick, test } from "../fixtures";

test("a protection rule opens in the form to change it", async ({ page }, info) => {
  const pattern = `change-rule/${info.project.name}-${info.repeatEachIndex}`;
  await signIn(page, USERS.alice);
  await page.goto("/alice/demo/settings/protections");
  await expect(page.getByRole("heading", { name: "Add a rule" })).toBeVisible();
  await page.getByLabel("Branch name or pattern").fill(pattern);
  await page.getByLabel("Approvals needed").fill("2");
  await tapOrClick(page, page.getByRole("checkbox", { name: "Require resolved threads" }));
  await tapOrClick(page, page.getByText("Stricter review for some changes"));
  await tapOrClick(page, page.getByRole("checkbox", { name: "Deletes tests" }));
  await page.getByLabel(/Required status checks/).fill("ci/build by alice\nlint");
  await tapOrClick(page, page.getByRole("button", { name: "Save rule" }));
  await expectConfirmed(page);

  const row = page.getByRole("row", { name: new RegExp(pattern) });
  await tapOrClick(page, row.getByRole("link", { name: `Change rule ${pattern}` }));
  await expect(page.getByRole("heading", { name: `Change the rule for ${pattern}` })).toBeVisible();
  await expect(page.getByLabel("Branch name or pattern")).toHaveValue(pattern);
  await expect(page.getByLabel("Approvals needed")).toHaveValue("2");
  await expect(page.getByRole("checkbox", { name: "Require resolved threads" })).toBeChecked();
  await expect(page.getByRole("checkbox", { name: "Block force pushes" })).toBeChecked();
  await expect(page.getByRole("checkbox", { name: "Require merge requests" })).not.toBeChecked();
  // The stricter review it asks for shows unfolded.
  await expect(page.getByRole("checkbox", { name: "Deletes tests" })).toBeChecked();
  await expect(page.getByLabel(/Required status checks/)).toHaveValue("ci/build by alice\nlint");
  await expectAccessible(page);

  // One change, then save: the rest stays as it was.
  await tapOrClick(page, page.getByRole("checkbox", { name: "Require resolved threads" }));
  await tapOrClick(page, page.getByRole("button", { name: "Save rule" }));
  await expectConfirmed(page);
  await expect(row).toContainText("2 approvals");
  await expect(row).not.toContainText("threads resolved");
  await expect(row).toContainText("a person approves when it deletes tests");
  await expect(row).toContainText("ci/build by alice; lint");

  await tapOrClick(page, row.getByRole("button", { name: `Remove rule ${pattern}` }));
  await expect(page.getByRole("row", { name: new RegExp(pattern) })).toHaveCount(0);
});

test("a save that fails keeps what was entered", async ({ page }, info) => {
  const pattern = `failed-rule/${info.project.name}-${info.repeatEachIndex}`;
  await signIn(page, USERS.alice);
  await page.goto("/alice/demo/settings/protections");
  await page.getByLabel("Branch name or pattern").fill(pattern);
  await page.getByLabel("Approvals needed").fill("99");
  await tapOrClick(page, page.getByRole("checkbox", { name: "Block deletion" }));
  await tapOrClick(page, page.getByRole("checkbox", { name: "Require the branch to be up to date with the target" }));
  await tapOrClick(page, page.getByText("Stricter review for some changes"));
  await tapOrClick(page, page.getByRole("checkbox", { name: "Adds binaries" }));
  await page.getByLabel(/Required status checks/).fill("ci/build");
  await tapOrClick(page, page.getByRole("button", { name: "Save rule" }));
  await expectProblem(page);
  await expect(page.getByLabel("Branch name or pattern")).toHaveValue(pattern);
  await expect(page.getByLabel("Approvals needed")).toHaveValue("99");
  await expect(page.getByRole("checkbox", { name: "Block deletion" })).not.toBeChecked();
  await expect(page.getByRole("checkbox", { name: "Require the branch to be up to date with the target" })).toBeChecked();
  await expect(page.getByRole("checkbox", { name: "Adds binaries" })).toBeChecked();
  await expect(page.getByLabel(/Required status checks/)).toHaveValue("ci/build");
  await expectAccessible(page);
  await expect(page.getByRole("row", { name: new RegExp(pattern) })).toHaveCount(0);
});
