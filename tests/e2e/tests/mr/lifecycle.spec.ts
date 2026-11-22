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
  // The banner for the branch "pushed" offers to open a merge request.
  const banner = page
    .locator(".notice-action")
    .filter({ has: page.getByRole("link", { name: "Create merge request" }) })
    .filter({ has: page.locator("code").getByText("pushed", { exact: true }) });
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
  await expect(page.locator("section[aria-labelledby=commits-heading] .item-list")).toContainText(`Add notes for ${p}`);
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
  // The kept name is a chip; removing it leaves no reviewer.
  await tapOrClick(page, page.getByRole("button", { name: "Remove nobody-here" }));
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
  // Closing is in the header's action menu.
  await tapOrClick(page, page.getByText("More actions"));
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

  // 5. Checkout commands for Git (the merge request ref) or jj, with copy
  // buttons, in the Check out menu: one tool at a time, preselected for
  // the viewer's tool (alice pushed with Git).
  await tapOrClick(page, page.locator("summary", { hasText: "Check out" }));
  await expect(page.locator("#checkout-git")).toBeVisible();
  await expect(page.locator("#checkout-git")).toHaveValue(
    `git fetch origin refs/merge-requests/${n}/head && git switch --detach FETCH_HEAD`,
  );
  await expect(page.getByRole("button", { name: "Copy the Git commands" })).toBeVisible();
  await expect(page.locator("#checkout-jj")).toBeHidden();
  await tapOrClick(page, page.getByRole("radio", { name: "jj" }));
  await expect(page.locator("#checkout-jj")).toBeVisible();
  await expect(page.locator("#checkout-jj")).toHaveValue(
    "jj git fetch --remote origin --branch review && jj new review@origin",
  );
  await expect(page.getByRole("button", { name: "Copy the jj commands" })).toBeVisible();
  await expect(page.locator("#checkout-git")).toBeHidden();
  await expectAccessible(page, "details.menu[open] .copy-panel");
  // Opened again, the page shows her tool's commands again.
  await page.goto(page.url());
  await tapOrClick(page, page.locator("summary", { hasText: "Check out" }));
  await expect(page.locator("#checkout-git")).toBeVisible();
  await page.keyboard.press("Escape");

  // 8. Both revisions in the timeline, the second with "Compare with previous".
  await expect(page.locator("#revision-1")).toContainText("opened revision 1 with 1 commit");
  await expect(page.locator("#revision-2")).toContainText("pushed revision 2 with 2 commits");
  await expect(page.locator("#revision-2")).toContainText(": 1 change added");
  await expectAccessible(page);
  await tapOrClick(page, page.locator("#revision-2").getByRole("link", { name: "Compare with previous" }));
  await expect(page.getByRole("note")).toContainText("Heuristic:");
  await expectAccessible(page);
  await tapOrClick(page, page.getByRole("link", { name: "Show the tree diff between both revisions" }));
  await expect(page.getByRole("note")).toBeVisible();
  await expect(page.locator(".diff-file")).toContainText("Second draft.");
  await expectAccessible(page);

  // The tabs count the current revision's changes and changed files.
  const tabs = page.getByRole("navigation", { name: "Merge request" });
  await expect(tabs.getByRole("link", { name: "Changes" }).locator(".count")).toHaveText("2");
  await tapOrClick(page, tabs.getByRole("link", { name: "Files" }));
  await expect(page).toHaveURL(/\/files$/);
  const files = tabs.getByRole("link", { name: "Files" }).locator(".count");
  await expect(files).toHaveText(/^[1-9]\d*$/);
  await expect(page.locator(".diff-file")).toHaveCount(Number(await files.textContent()));
  await tapOrClick(page, tabs.getByRole("link", { name: "Changes" }));
  await expect(page).toHaveURL(/\/changes$/);
  await expect(page.getByRole("heading", { name: "2 changes" })).toBeVisible();
  const rows = page.locator("section[aria-labelledby=changes-heading] .item-list > li");
  await expect(rows.first()).toBeVisible();
  await expect(page.locator("section[aria-labelledby=changes-heading]")).toContainText("Revise review notes");
  // Each change shows its ID (jj's format); the second one is new.
  await expect(rows).toHaveCount(2);
  await expect(rows.nth(0).locator(".change-id")).toHaveText(/^[k-z]{8}$/);
  // How a commit was matched shows in the change's history, not here.
  await expect(rows.locator(".badge")).toHaveCount(0);
  await expectAccessible(page);
  // Each change has its own page with its versions.
  await tapOrClick(page, rows.nth(0).locator("a.item-title"));
  await expect(page.getByRole("heading", { name: /^Change [k-z]{8}$/ })).toBeVisible();
  const versions = page.getByRole("list", { name: "Versions" });
  await expect(versions.getByRole("listitem")).toHaveCount(2);
  await expect(versions.getByRole("listitem").first()).toContainText("First version.");
  await expect(versions.getByRole("listitem").first()).toContainText("Revision 1");
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
  const box = page.getByRole("region", { name: "Ready to merge", exact: true });
  await expect(page.getByRole("region", { name: "Merge box: Ready to merge" })).toBeVisible();
  // Every requirement is met: none blocks.
  await expect(box.locator("li.requirement")).not.toHaveCount(0);
  await expect(box.locator("li.requirement.tone-blocking")).toHaveCount(0);
  // Mergeable: the card's one action leads to the merge form, which offers
  // the repository's strategies.
  await expect(box.getByRole("link", { name: "Merge" })).toHaveAttribute("href", /#merge-form$/);
  await expect(page.getByLabel("Merge with")).toBeVisible();
  await expect(page.getByRole("button", { name: "Merge", exact: true })).toBeVisible();
  // One primary action per page.
  await expect(page.locator(".button-primary")).toHaveCount(1);
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

test("conf_review_19_the_review_form_can_leave_the_decision_to_someone", async ({ page }) => {
  // Not the author: the admin may approve, and so may defer.
  await signIn(page, USERS.admin);
  await page.goto("/alice/demo/merge-requests");
  await tapOrClick(page, page.getByRole("link", { name: "Review fixture" }));
  const option = page.getByLabel("Approve, but let someone else decide");
  const field = page.getByLabel("Who decides");
  // "Who decides" shows only while its option is chosen.
  await expect(field).toBeHidden();
  await tapOrClick(page, option);
  await expect(field).toBeVisible();
  await expectAccessible(page);
  await tapOrClick(page, page.getByLabel("Approve", { exact: true }));
  await expect(field).toBeHidden();
  // Keyboard only: arrow keys move through the radios to the option, and
  // Tab then reaches the field.
  await page.getByLabel("Approve", { exact: true }).focus();
  await page.keyboard.press("ArrowDown");
  await expect(option).toBeChecked();
  await page.keyboard.press("Tab");
  await expect(field).toBeFocused();
  // It suggests people and the organization's teams; choosing one puts
  // just that name in the field. (The fixture's repository belongs to a
  // person, so the team comes from a stand-in answer.)
  let teamQuery: string | null = null;
  await page.route("**/-/ui/references?*kind=team*", async (route) => {
    teamQuery = new URL(route.request().url()).searchParams.get("q");
    await route.fulfill({
      json: { items: [{ kind: "team", value: "@acme/leads", label: "acme/leads", detail: null, color: null, avatar: null }] },
    });
  });
  await field.fill("al");
  const list = page.getByRole("listbox", { name: "People and teams" });
  await expect(list.getByRole("option").filter({ hasText: "alice" })).toBeVisible();
  await expect(list.getByRole("option").filter({ hasText: "acme/leads" })).toBeVisible();
  expect(teamQuery).toBe("al");
  await page.keyboard.press("ArrowDown");
  await page.keyboard.press("Enter");
  await expect(field).toHaveValue("acme/leads");
  await expect(list).toBeHidden();
  await field.fill("acme/le");
  await expect(list.getByRole("option").filter({ hasText: "acme/leads" })).toBeVisible();
  expect(teamQuery).toBe("le");
  await page.keyboard.press("Escape");
  await page.unroute("**/-/ui/references?*kind=team*");
  // Leaving it to the author is refused next to the field, which stays
  // shown with the name; nothing is recorded, so the shared fixture stays.
  await field.fill("alice");
  await tapOrClick(page, page.getByRole("button", { name: "Submit review" }));
  await expect(page.locator("#review-defer-error")).toHaveText("The author can't decide on their own merge request.");
  await expect(page.getByLabel("Approve, but let someone else decide")).toBeChecked();
  await expect(page.getByLabel("Who decides")).toHaveValue("alice");
  await expect(page.getByLabel("Who decides")).toHaveAttribute("aria-invalid", "true");
  await expectAccessible(page);
});

test("conf_a11y_01_threads_in_the_conversation", async ({ page }, testInfo) => {
  await signIn(page, USERS.alice);
  await page.goto("/alice/demo/merge-requests");
  await tapOrClick(page, page.getByRole("link", { name: "Review fixture" }));
  // Other tests add threads on the same line; this is the seeded one.
  const seeded = () =>
    page.getByRole("article", { name: "Thread on review.md, line 1" }).filter({ hasText: "Is this the final wording?" });
  await expect(seeded()).toBeVisible();
  await expect(seeded().getByRole("region", { name: "Code at review.md, line 1" })).toContainText("Second draft.");
  // A suggestion shows what it changes, with its actions.
  const suggestion = page.getByRole("article", { name: "Thread on review.md, line 1" }).filter({ hasText: "Shorter:" });
  await expect(suggestion.getByRole("region", { name: "Suggested change" })).toContainText("Second.");
  await expect(suggestion.getByRole("button", { name: "Apply suggestion" })).toBeVisible();
  await expect(page.getByRole("heading", { name: "Apply suggestions" })).toBeVisible();
  await expectAccessible(page);
  // The filter narrows the conversation to unresolved threads.
  await tapOrClick(page, page.locator("summary", { hasText: "Show:" }));
  await tapOrClick(page, page.getByRole("list", { name: "Show" }).getByRole("link", { name: /Unresolved/ }));
  await expect(page).toHaveURL(/show=unresolved/);
  await expect(seeded()).toBeVisible();
  await expect(page.getByRole("article", { name: "Description" })).toBeVisible();
  await expectAccessible(page);
  // A reply lands in the thread.
  const text = `Reply from ${testInfo.project.name}`;
  await expect(seeded().getByLabel("Your reply")).toBeHidden();
  await tapOrClick(page, seeded().locator("summary", { hasText: "Reply" }));
  await expect(seeded().getByLabel("Your reply")).toBeFocused();
  await seeded().getByLabel("Your reply").fill(text);
  await tapOrClick(page, seeded().getByRole("button", { name: "Send reply" }));
  await expect(seeded()).toContainText(text);
});

test("conf_review_reply_resolves_its_thread", async ({ page }, testInfo) => {
  test.slow();
  await signIn(page, USERS.alice);
  await page.goto("/alice/demo/merge-requests");
  await tapOrClick(page, page.getByRole("link", { name: "Review fixture" }));
  await expect(page).toHaveURL(/\/merge-requests\/\d+$/);
  const conversation = page.url();
  // A thread of this browser's own, started on a line.
  await tapOrClick(page, page.getByRole("navigation", { name: "Merge request" }).getByRole("link", { name: "Files" }));
  await tapOrClick(page, page.locator(".diff-unified a[title='Comment on this line']").first());
  const note = `Resolve me on ${testInfo.project.name}`;
  await page.getByLabel("Comment", { exact: true }).fill(note);
  await tapOrClick(page, page.getByRole("button", { name: "Comment now" }));
  await expect(page.locator(".diff-thread-row").filter({ hasText: note })).toBeVisible();
  await page.goto(conversation);
  const mine = page.locator("li.timeline-thread").filter({ hasText: note });
  await expect(mine.getByRole("button", { name: "Resolve thread" })).toBeVisible();
  // The reply box resolves while replying; its own button stands aside.
  await tapOrClick(page, mine.locator("summary", { hasText: "Reply" }));
  await expect(mine.getByRole("button", { name: "Resolve thread" })).toBeHidden();
  await mine.getByLabel("Your reply").fill("Done.");
  await mine.getByRole("checkbox", { name: /^Resolve thread/ }).check();
  await expectAccessible(page);
  await tapOrClick(page, mine.getByRole("button", { name: "Send reply" }));
  const done = page.locator("li.timeline-thread").filter({ hasText: note });
  await expect(done.locator(".comment-header")).toContainText("Resolved");
  await expect(done).toContainText("Done.");
  await expect(done.getByRole("button", { name: "Reopen thread" })).toBeVisible();
});

test("conf_agent_02_findings_show_as_bot_threads", async ({ page }) => {
  await signIn(page, USERS.alice);
  await page.goto("/alice/demo/merge-requests");
  await tapOrClick(page, page.getByRole("link", { name: "Review fixture" }));
  const finding = page
    .getByRole("article", { name: "Thread on review.md, line 1" })
    .filter({ hasText: "Finding from lint (warning)" });
  await expect(finding).toBeVisible();
  await expect(finding.locator(".thread-comment-head").first()).toContainText("e2e-lint");
  await expect(finding.locator(".thread-comment-head").first()).toContainText("Bot");
  await expect(finding.locator(".finding-message")).toHaveText("Say what changed in the draft.\nKeep it to one line.");
  await expectAccessible(page);
  // The Files tab shows it under its line.
  await page.goto(page.url().replace(/#.*$/, "") + "/files");
  await expect(page.locator(".inline-thread").filter({ hasText: "Finding from lint (warning)" })).toBeVisible();
  await expectAccessible(page);
});

test("conf_a11y_04_line_comments_on_the_changes_tab", async ({ page }, testInfo) => {
  test.slow();
  await signIn(page, USERS.alice);
  await page.goto("/alice/demo/merge-requests");
  await tapOrClick(page, page.getByRole("link", { name: "Review fixture" }));
  await tapOrClick(page, page.getByRole("navigation", { name: "Merge request" }).getByRole("link", { name: "Files" }));
  // The seeded thread sits under its line.
  await expect(page.locator(".diff-thread-row").first()).toContainText("Is this the final wording?");
  await expectAccessible(page);
  // Tapping a line number opens a comment form under that line.
  await tapOrClick(page, page.locator(".diff-unified a[title='Comment on this line']").first());
  await expect(page).toHaveURL(/comment=new:1:review\.md/);
  const text = `Inline note from ${testInfo.project.name}`;
  await page.getByLabel("Comment", { exact: true }).fill(text);
  await expect(page.getByLabel(/Comment on lines 1 to/)).toHaveValue("1");
  await expectAccessible(page);
  await tapOrClick(page, page.getByRole("button", { name: "Add to my review" }));
  const mine = page.locator(".diff-thread-row").filter({ hasText: text });
  await expect(mine).toContainText("Pending");
});

test("conf_agent_09_a_bot_merge_request_iterates", async ({ page }) => {
  await signIn(page, USERS.alice);
  await page.goto("/alice/demo/merge-requests");
  const row = page.locator(".item-row").filter({ hasText: "Agent fixture" });
  await expect(row.locator(".badge", { hasText: "Iterating" })).toBeVisible();
  await tapOrClick(page, row.getByRole("link", { name: "Agent fixture" }));
  await expect(page.locator(".state-label")).toHaveText("Iterating");
  // The readiness card says what's next, without doing it here (the
  // fixture stays iterating for every project).
  const card = page.locator(".readiness").first();
  await expect(card).toContainText("Iterating");
  await expect(card.getByRole("button", { name: "Mark as ready" })).toBeVisible();
  // More offers both: ready, or a draft that keeps iterating.
  const more = page.locator("details.menu").filter({ has: page.locator("summary", { hasText: "More actions" }) });
  await tapOrClick(page, more.locator("summary"));
  await expect(more.getByRole("button", { name: "Mark as ready" })).toBeVisible();
  await expect(more.getByRole("button", { name: "Mark as draft" })).toBeVisible();
  // Only the open menu: its panel covers the page below it.
  await expectAccessible(page, "details.menu[open]");
  await tapOrClick(page, more.locator("summary"));
  // The bot's three revisions are one timeline entry, each still listed.
  const group = page.locator(".timeline-group").filter({ hasText: "pushed 3 iterations (revisions 1–3)" });
  await expect(group).toBeVisible();
  // A chevron says it opens.
  await expect(group.locator("summary .disclosure")).toBeVisible();
  await expectAccessible(page);
  await tapOrClick(page, group.locator("summary"));
  await expect(group.locator("#revision-2")).toContainText("pushed revision 2");
  await expect(group.locator("#revision-3").getByRole("link", { name: "Compare with previous" })).toBeVisible();
  await expectAccessible(page);
});

test("conf_agent_12_a_bot_marks_a_persons_thread_addressed", async ({ page }) => {
  await signIn(page, USERS.alice);
  await page.goto("/alice/demo/merge-requests");
  await tapOrClick(page, page.getByRole("link", { name: "Agent fixture" }));
  const thread = page.getByRole("article", { name: "Thread on agent-notes.txt, line 1" });
  await expect(thread.locator(".comment-header")).toContainText(/marked as addressed by\s*e2e-agent/);
  await expect(thread.locator(".comment-header")).toContainText("Bot");
  // Still open: a person resolves it.
  await expect(thread.getByRole("button", { name: "Resolve thread" })).toBeVisible();
  await expectAccessible(page);
  await page.goto(page.url().replace(/#.*$/, "") + "/files");
  await expect(page.locator(".inline-thread").filter({ hasText: /Marked as addressed by\s*e2e-agent/ })).toBeVisible();
  await expectAccessible(page);
});
