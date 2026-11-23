// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//
// Working alone: a branch protection rule with the one-maintainer policy,
// then alice reviews her own merge request as the only maintainer and
// merges it, in every browser cell. The e2e server seeds
// `solo/<project>/main` (the protected target) and `solo/<project>/topic`.
import { USERS, expect, expectAccessible, expectConfirmed, signIn, tapOrClick, test } from "../../fixtures";

test("conf_review_32_review_alone_and_merge_under_the_policy", async ({ page }, info) => {
  const project = info.project.name;
  await signIn(page, USERS.alice);
  // The rule: one approval, and the one-maintainer policy.
  await page.goto("/alice/demo/settings/protections");
  await page.getByLabel("Branch name or pattern").fill(`solo/${project}/main`);
  await tapOrClick(page, page.getByLabel("Only one maintainer: their own review counts"));
  await expectAccessible(page);
  await tapOrClick(page, page.getByRole("button", { name: "Save rule" }));
  await expectConfirmed(page);
  await expect(page.getByRole("row", { name: new RegExp(`solo/${project}/main`) })).toContainText("one-maintainer policy");
  // Her own merge request: nobody else can approve it.
  await page.goto(
    `/alice/demo/compare?source=${encodeURIComponent(`solo/${project}/topic`)}&target=${encodeURIComponent(`solo/${project}/main`)}`,
  );
  await page.getByLabel("Title").fill(`Alone in ${project}`);
  await tapOrClick(page, page.getByRole("button", { name: "Create merge request" }));
  await expect(page.getByRole("heading", { level: 1 })).toContainText(`Alone in ${project}`);
  const card = page.locator("section.readiness").first();
  await expect(card).toContainText("Under the one-maintainer policy, alice's review of this revision counts.");
  // The card's next step is the review form, with the option to review alone.
  await expect(card.getByRole("link", { name: "Review" })).toHaveAttribute("href", /#review-form$/);
  const form = page.locator("#review-form");
  const alone = form.getByRole("radio", { name: /^Review as the only maintainer/ });
  await alone.check();
  await expect(alone).toBeChecked();
  await expectAccessible(page);
  await tapOrClick(page, form.getByRole("button", { name: "Submit review" }));
  await expect(page.getByText("reviewed as the only maintainer").first()).toBeVisible();
  await expect(page.locator("section.readiness").first()).toContainText("Ready to merge");
  await tapOrClick(page, page.locator("#merge-form").getByRole("button", { name: "Merge" }));
  await expect(page.getByText("merged this under the one-maintainer policy")).toBeVisible();
  await expectAccessible(page);
});
