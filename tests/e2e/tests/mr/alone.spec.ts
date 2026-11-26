// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//
// Working alone: a branch protection rule that needs an approval nobody
// else can give says so, in the settings and on the merge request, whose
// next step leads to the one-maintainer policy. With it on, alice reviews a
// merge request of alice's own as the only maintainer and merges it, in every
// browser cell. The e2e server seeds `solo/<project>/main` (the protected
// target) and `solo/<project>/topic`.
import type { Page } from "@playwright/test";
import { USERS, expect, expectAccessible, expectConfirmed, signIn, tapOrClick, test } from "../../fixtures";

/** Saves the rule for `pattern`, with or without the policy. */
async function saveRule(page: Page, pattern: string, policy: boolean): Promise<void> {
  await page.getByLabel("Branch name or pattern").fill(pattern);
  const box = page.getByLabel("Only one maintainer: their own review counts");
  if (policy) {
    await box.check();
  }
  await tapOrClick(page, page.getByRole("button", { name: "Save rule" }));
  await expectConfirmed(page);
}

test("conf_review_32_review_alone_and_merge_under_the_policy", async ({ page }, info) => {
  // A long flow: webkit on the CI runners needs more than 30 s.
  test.slow();
  const project = info.project.name;
  const target = `solo/${project}/main`;
  await signIn(page, USERS.alice);
  // The form says that nobody else could approve.
  await page.goto("/alice/demo/settings/protections");
  await expect(page.getByText(/^Only alice has write access, so nobody else can approve alice's merge requests\. With approvals needed/)).toBeVisible();
  await saveRule(page, target, false);
  await expect(page.getByText(`${target}: only alice has write access`)).toBeVisible();
  await expectAccessible(page);
  // Her own merge request: stuck, and the card says what helps.
  await page.goto(`/alice/demo/compare?source=${encodeURIComponent(`solo/${project}/topic`)}&target=${encodeURIComponent(target)}`);
  await page.getByLabel("Title").fill(`Alone in ${project}`);
  await tapOrClick(page, page.getByRole("button", { name: "Create merge request" }));
  await expect(page.getByRole("heading", { level: 1 })).toContainText(`Alone in ${project}`);
  const mr = page.url();
  const card = page.locator("section.readiness").first();
  await expect(card).toContainText("Nobody else with write access can approve it.");
  await expectAccessible(page);
  await tapOrClick(page, card.getByRole("link", { name: "Turn on the one-maintainer policy" }));
  await expect(page.getByRole("heading", { level: 1 })).toHaveText("Branch protection");
  await saveRule(page, target, true);
  await expect(page.getByRole("row", { name: new RegExp(target) })).toContainText("one-maintainer policy");
  await expect(page.getByText(`${target}: only alice has write access`)).toHaveCount(0);
  // Now alice's review counts.
  await page.goto(mr);
  await expect(card).toContainText("Under the one-maintainer policy, alice's review of this revision counts.");
  await expect(card.getByRole("link", { name: "Review" })).toHaveAttribute("href", /#review-form$/);
  const form = page.locator("#review-form");
  const alone = form.getByRole("radio", { name: /^Review as the only maintainer/ });
  await alone.check();
  await expect(alone).toBeChecked();
  await expectAccessible(page);
  await tapOrClick(page, form.getByRole("button", { name: "Submit review" }));
  await expect(page.getByText("reviewed as the only maintainer").first()).toBeVisible();
  await expect(card).toContainText("Ready to merge");
  await tapOrClick(page, page.locator("#merge-form").getByRole("button", { name: "Merge" }));
  await expect(page.getByText("merged this under the one-maintainer policy")).toBeVisible();
  await expectAccessible(page);
});
