// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
import { USERS, editor, expect, expectAccessible, sideMenu, signIn, tapOrClick, test, write } from "../fixtures";

test("conf_a11y_01_issue_pages_accessible", async ({ page }, testInfo) => {
  test.slow();
  await signIn(page, USERS.alice);
  await page.goto("/alice/demo");
  await tapOrClick(page, (await sideMenu(page, "Repository")).getByRole("link", { name: "Issues" }));
  await expect(page.getByRole("heading", { level: 1 })).toHaveText("Issues");
  await expectAccessible(page);
  await tapOrClick(page, page.getByRole("link", { name: "New issue" }));
  await expect(page.getByRole("heading", { level: 1 })).toHaveText("New issue");
  await expectAccessible(page);
  // Submitting without a title shows the error summary.
  await tapOrClick(page, page.getByRole("button", { name: "Open issue" }));
  await expect(page.getByRole("alert")).toContainText("Enter a title.");
  await expectAccessible(page);
  const title = `Broken on ${testInfo.project.name}`;
  await page.getByLabel("Title").fill(title);
  await write(page, "Description", "It fails with bold text.");
  await tapOrClick(page, page.getByRole("button", { name: "Open issue" }));
  await expect(page.getByRole("heading", { level: 1 })).toContainText(title);
  await expectAccessible(page);
  await write(page, "Add a comment", "A comment.");
  await tapOrClick(page, page.getByRole("button", { name: "Comment", exact: true }));
  await expect(page.getByText("A comment.")).toBeVisible();
  await tapOrClick(page, page.getByRole("button", { name: "Close issue" }));
  await expect(page.getByText("closed this as completed")).toBeVisible();
  await expectAccessible(page);
  await tapOrClick(page, page.getByRole("link", { name: "Edit issue", exact: true }));
  await expect(page.getByRole("heading", { level: 1 })).toContainText("Edit issue");
  await expectAccessible(page);
});

test("conf_a11y_01_label_and_milestone_pages_accessible", async ({ page }, testInfo) => {
  test.slow();
  const p = testInfo.project.name;
  await signIn(page, USERS.alice);
  await page.goto("/alice/demo/labels");
  await expect(page.getByRole("heading", { level: 1 })).toHaveText("Labels");
  await expectAccessible(page);
  // An invalid label shows the error summary.
  await tapOrClick(page, page.getByRole("button", { name: "Create label" }));
  await expect(page.getByRole("alert")).toContainText("Enter a name.");
  await expectAccessible(page);
  const label = `triage-${p}`;
  await page.getByLabel("Name").fill(label);
  await page.getByLabel("Color").fill("#d73a4a");
  await tapOrClick(page, page.getByRole("button", { name: "Create label" }));
  await expect(page.locator(".success-summary")).toContainText("Saved.");
  await expect(page.getByRole("link", { name: label, exact: true })).toBeVisible();
  await expectAccessible(page);

  await page.goto("/alice/demo/milestones");
  await expect(page.getByRole("heading", { level: 1 })).toHaveText("Milestones");
  const milestone = `Release ${p}`;
  await page.getByLabel("Title").fill(milestone);
  await page.getByLabel(/Due date/).fill("2026-12-31");
  await tapOrClick(page, page.getByRole("button", { name: "Create milestone" }));
  await expect(page.getByRole("link", { name: milestone, exact: true })).toBeVisible();
  await expectAccessible(page);

  // Triage an issue from the sidebar.
  await page.goto("/alice/demo/issues/new");
  const title = `Triage on ${p}`;
  await page.getByLabel("Title").fill(title);
  await tapOrClick(page, page.getByRole("button", { name: "Open issue" }));
  await expect(page.getByRole("heading", { level: 1 })).toContainText(title);
  await page.getByRole("checkbox", { name: label, exact: true }).check();
  await tapOrClick(page, page.getByRole("button", { name: "Save labels" }));
  await expect(page.getByText(`added the label ${label}`)).toBeVisible();
  await page.getByLabel("Set milestone").selectOption(milestone);
  await tapOrClick(page, page.getByRole("button", { name: "Save milestone" }));
  await expect(page.getByText(`added this to the milestone ${milestone}`)).toBeVisible();
  await page.getByLabel("Assign people").fill("alice");
  await tapOrClick(page, page.getByRole("button", { name: "Save assignees" }));
  await expect(page.getByText("assigned alice")).toBeVisible();
  await expectAccessible(page);

  // Filtering by the label finds exactly this issue.
  await tapOrClick(page, page.getByRole("complementary").getByRole("link", { name: label, exact: true }));
  await expect(page.getByRole("heading", { level: 1 })).toHaveText("Issues");
  await expect(page.getByRole("link", { name: title, exact: true })).toBeVisible();
  await expect(page.locator(".item-row")).toHaveCount(1);
  await expectAccessible(page);
});

test("issue_page_actions_assignee_suggestions_and_comment_and_close", async ({ page }, testInfo) => {
  test.slow();
  await signIn(page, USERS.alice);
  // New repositories come with a set of labels.
  await page.goto("/alice/demo/labels");
  await expect(page.getByText("good first issue", { exact: true })).toBeVisible();
  await page.goto("/alice/demo/issues/new");
  const title = `Shortcuts ${testInfo.project.name}`;
  await page.getByLabel("Title").fill(title);
  await tapOrClick(page, page.getByRole("button", { name: "Open issue" }));
  await expect(page.getByRole("heading", { level: 1 })).toContainText(title);
  // The author is subscribed: only "Unsubscribe" is offered.
  const notifications = page.getByRole("complementary", { name: "Issue details" });
  await expect(notifications.getByRole("button", { name: "Unsubscribe" })).toBeVisible();
  await expect(notifications.getByRole("button", { name: "Subscribe", exact: true })).toHaveCount(0);
  // Assignees: suggestions while typing.
  const assignees = page.getByRole("combobox", { name: "Assign people" });
  await assignees.fill("");
  await assignees.pressSequentially("ali");
  const people = page.getByRole("listbox", { name: "People" });
  await expect(people.getByRole("option").first()).toContainText("alice");
  await expectAccessible(page);
  await assignees.press("Enter");
  await expect(assignees).toHaveValue("alice, ");
  await tapOrClick(page, page.getByRole("button", { name: "Save assignees" }));
  await expect(page.getByRole("complementary", { name: "Issue details" }).getByRole("link", { name: "alice" })).toBeVisible();
  // Comment and close with Ctrl+Shift+Enter.
  await expect(page.locator("slath-editor[data-state=ready]")).toBeVisible();
  await write(page, "Add a comment", "Done here.");
  await expect(editor(page, "Add a comment")).toContainText("Done here.");
  await page.keyboard.press("ControlOrMeta+Shift+Enter");
  await expect(page.getByText("Done here.")).toBeVisible();
  await expect(page.getByText("closed this as completed")).toBeVisible();
  await expect(page.getByRole("button", { name: /Comment and reopen/ })).toBeVisible();
});

test("repository_watch_toggle", async ({ page }) => {
  await signIn(page, USERS.alice);
  await page.goto("/alice/demo");
  // One button that says what it does; no "Ignore". (Other tests share the
  // account and may toggle it meanwhile, so only the shape is checked.)
  const buttons = page.locator(".repo-actions").getByRole("button");
  await expect(buttons).toHaveCount(1);
  await expect(buttons).toHaveText(/^(Watch|Unwatch)$/);
  await expect(page.getByRole("button", { name: "Ignore" })).toHaveCount(0);
  // Repository-wide actions are on every tab, not only on the front page.
  for (const path of ["/alice/demo/issues", "/alice/demo/commits", "/alice/demo/labels"]) {
    await page.goto(path);
    await expect(page.locator(".repo-actions").getByRole("button")).toHaveText(/^(Watch|Unwatch)$/);
  }
  // Issues, Labels, and Milestones are a side menu, like settings sections.
  const sections = page.getByRole("navigation", { name: "Issues" });
  await expect(sections).toHaveClass(/tabs/);
  await expect(sections.getByRole("link", { name: "Labels" })).toHaveAttribute("aria-current", "page");
  // The top bar shows the avatar, named and titled with the user name.
  const me = page.getByRole("navigation", { name: "Main" }).getByRole("link", { name: "alice", exact: true });
  await expect(me).toHaveAttribute("title", "alice");
  await expect(me.locator("img")).toBeVisible();
});
