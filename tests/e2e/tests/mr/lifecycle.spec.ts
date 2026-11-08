// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//
// Merge requests: from a push to a merge request, the compare page, the
// checkout commands, and revisions in the timeline. The e2e server seeds a
// branch `e2e/<project>` per Playwright project and the merge request
// "Review fixture" with two revisions.
import { USERS, expect, expectAccessible, signIn, tapOrClick, test, write } from "../../fixtures";

test("ux_mr_01_recently_pushed_banner_on_home_and_branches", async ({ page }) => {
  await signIn(page, USERS.alice);
  const banner = page.locator(".recent-push").filter({ hasText: "You pushed to pushed recently." });
  for (const path of ["/alice/demo/branches", "/alice/demo"]) {
    await page.goto(path);
    await expect(banner).toBeVisible();
    await expectAccessible(page);
  }
  await tapOrClick(page, banner.getByRole("link", { name: "Create merge request" }));
  await expect(page).toHaveURL(/\/alice\/demo\/compare\?source=pushed$/);
  await expect(page.getByRole("heading", { level: 1 })).toHaveText("New merge request");
  await expect(page.getByLabel("Title")).toHaveValue("Add a pushed note");
});

test("ux_mr_02_compare_page_creates_a_merge_request", async ({ page }, testInfo) => {
  test.slow();
  const p = testInfo.project.name;
  const branch = `e2e/${p}`;
  await signIn(page, USERS.alice);
  // The page the push hint links to.
  await page.goto(`/alice/demo/compare?source=${encodeURIComponent(branch)}`);

  // 2. The compare page: commits, the diff, a prefilled title, the pickers.
  await expect(page.getByRole("heading", { level: 1 })).toHaveText("New merge request");
  await expect(page.getByLabel("Merge", { exact: true })).toHaveValue(branch);
  await expect(page.getByLabel("into")).toHaveValue("main");
  await expect(page.getByRole("heading", { name: "1 commit" })).toBeVisible();
  await expect(page.locator(".commit-list")).toContainText(`Add notes for ${p}`);
  await expect(page.getByRole("heading", { name: "1 changed file" })).toBeVisible();
  await expect(page.locator(".diff-file")).toContainText(`# Notes for ${p}`);
  await expect(page.getByLabel("Title")).toHaveValue(`Add notes for ${p}`);
  await expect(page.getByLabel("Reviewers")).toBeVisible();
  await expect(page.getByLabel("Labels")).toBeVisible();
  await expect(page.getByLabel("Create as draft")).not.toBeChecked();
  await expectAccessible(page);

  // An unknown reviewer is reported next to the field; the input is kept.
  await page.getByLabel("Reviewers").fill("nobody-here");
  await write(page, "Description", "Notes for this browser.");
  await tapOrClick(page, page.getByRole("button", { name: "Create merge request" }));
  await expect(page.getByRole("alert")).toContainText("nobody-here isn't a user who can read this repository.");
  await expect(page.getByLabel("Title")).toHaveValue(`Add notes for ${p}`);
  await expectAccessible(page);
  await page.getByLabel("Reviewers").fill("");
  await page.getByLabel("Create as draft").check();
  await tapOrClick(page, page.getByRole("button", { name: "Create merge request" }));

  await expect(page.getByRole("heading", { level: 1 })).toContainText(`Add notes for ${p}`);
  await expect(page.locator(".state-label")).toHaveText("Draft");
  await expect(page.getByText("Notes for this browser.")).toBeVisible();
  await expectAccessible(page);
  await tapOrClick(page, page.getByRole("button", { name: "Mark as ready" }));
  await expect(page.locator(".state-label")).toHaveText("Open");
  await expect(page.getByText("marked this as ready for review")).toBeVisible();

  // With a merge request open, the compare page links to it.
  await page.goto(`/alice/demo/compare?source=${encodeURIComponent(branch)}`);
  await expect(page.getByRole("note")).toContainText("is already open");

  // Closing it from the list's merge request page.
  await page.goto("/alice/demo/merge-requests");
  await tapOrClick(page, page.getByRole("link", { name: `Add notes for ${p}` }));
  await tapOrClick(page, page.getByRole("button", { name: "Close merge request" }));
  await expect(page.locator(".state-label")).toHaveText("Closed");
  await expect(page.getByText("closed this")).toBeVisible();
});

test("ux_mr_05_08_checkout_commands_and_revisions_in_the_timeline", async ({ page }) => {
  test.slow();
  await signIn(page, USERS.alice);
  await page.goto("/alice/demo/merge-requests");
  await expect(page.getByRole("heading", { level: 1 })).toHaveText("Merge requests");
  await expectAccessible(page);
  await tapOrClick(page, page.getByRole("link", { name: "Review fixture" }));
  await expect(page.getByRole("heading", { level: 1 })).toContainText("Review fixture");
  const n = new URL(page.url()).pathname.split("/").pop();

  // 5. Checkout commands for Git (the merge request ref) and jj, with copy buttons.
  await expect(page.locator("#checkout-git")).toHaveText(
    `git fetch origin refs/merge-requests/${n}/head && git switch --detach FETCH_HEAD`,
  );
  await expect(page.locator("#checkout-jj")).toHaveText(
    "jj git fetch --remote origin --branch review && jj new review@origin",
  );
  await expect(page.getByRole("button", { name: "Copy Git commands" })).toBeVisible();
  await expect(page.getByRole("button", { name: "Copy jj commands" })).toBeVisible();

  // 8. Both revisions in the timeline, the second with "Compare with previous".
  await expect(page.locator("#revision-1")).toContainText("opened revision 1 with 1 commit");
  await expect(page.locator("#revision-2")).toContainText("pushed revision 2 with 2 commits");
  await expect(page.locator("#revision-2")).toContainText(": 1 change added");
  await expectAccessible(page);
  await tapOrClick(page, page.locator("#revision-2").getByRole("link", { name: "Compare with previous" }));
  await expect(page.getByRole("note")).toContainText("Heuristic:");
  await expectAccessible(page);
  await tapOrClick(page, page.getByRole("link", { name: "Show the tree diff between both revisions" }));
  await expect(page.getByRole("note")).toContainText("Changes from revision 1 to revision 2.");
  await expect(page.locator(".diff-file")).toContainText("Second draft.");
  await expectAccessible(page);

  await tapOrClick(
    page,
    page.getByRole("navigation", { name: "Merge request" }).getByRole("link", { name: "Commits" }),
  );
  await expect(page.getByRole("heading", { name: "2 commits" })).toBeVisible();
  await expect(page.locator(".commit-list")).toContainText("Revise review notes");
  // Each commit shows its change ID (jj's format); the second one is new.
  const rows = page.locator(".change-list > li");
  await expect(rows).toHaveCount(2);
  await expect(rows.nth(0).locator(".change-id")).toHaveText(/^[k-z]{8}$/);
  // How a commit was matched shows in the change's history, not here.
  await expect(rows.locator(".badge")).toHaveCount(0);
  await expectAccessible(page);
  // Each change has its own page with its versions.
  await tapOrClick(page, rows.nth(0).locator("a.change-id"));
  await expect(page.getByRole("heading", { name: /^Change [k-z]{8}$/ })).toBeVisible();
  await expect(page.getByRole("region", { name: "Versions" }).getByRole("row")).toHaveCount(3);
  await expect(page.getByRole("region", { name: "Versions" })).toContainText("first version");
  await expectAccessible(page);

  // Issue URLs of a merge request lead to it.
  await page.goto(`/alice/demo/issues/${n}`);
  await expect(page).toHaveURL(new RegExp(`/alice/demo/merge-requests/${n}$`));
});

test("conf_a11y_01_merge_box_review_form_and_review_requests", async ({ page }, testInfo) => {
  await signIn(page, USERS.alice);
  await page.goto("/alice/demo/merge-requests");
  await tapOrClick(page, page.getByRole("link", { name: "Review fixture" }));
  // No protection on main: nothing but the basics is required.
  const box = page.getByRole("region", { name: "Ready to merge" });
  await expect(box).toContainText("No approvals are required.");
  await expect(box).toContainText("Every commit belongs to one change.");
  // Mergeable: the merge form offers the repository's strategies.
  await expect(box.getByLabel("Merge with")).toBeVisible();
  await expect(box.getByRole("button", { name: "Merge" })).toBeVisible();
  // The author can comment, not approve.
  await expect(page.getByLabel("Comment", { exact: true })).toBeChecked();
  await expect(page.getByLabel("Approve")).toHaveCount(0);
  await expectAccessible(page);
  const note = `Reviewed in ${testInfo.project.name}.`;
  await write(page, "Review comment", note);
  await tapOrClick(page, page.getByRole("button", { name: "Submit review" }));
  await expect(page.getByRole("article", { name: "Review by alice" }).filter({ hasText: note })).toBeVisible();

  await page.goto("/-/review-requests");
  await expect(page.getByRole("heading", { level: 1 })).toHaveText("Review requests");
  await expectAccessible(page);
});
