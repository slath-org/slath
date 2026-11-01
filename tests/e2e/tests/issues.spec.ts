// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
import { USERS, expect, expectAccessible, signIn, tapOrClick, test } from "../fixtures";

test("conf_a11y_01_issue_pages_accessible", async ({ page }, testInfo) => {
  test.slow();
  await signIn(page, USERS.alice);
  await page.goto("/alice/demo");
  await tapOrClick(page, page.getByRole("navigation", { name: "Repository" }).getByRole("link", { name: "Issues" }));
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
  await page.getByLabel("Description").fill("It fails with **bold** text and #1.");
  await tapOrClick(page, page.getByRole("button", { name: "Open issue" }));
  await expect(page.getByRole("heading", { level: 1 })).toContainText(title);
  await expectAccessible(page);
  await page.getByLabel("Add a comment").fill("A comment.");
  await tapOrClick(page, page.getByRole("button", { name: "Comment", exact: true }));
  await expect(page.getByText("A comment.")).toBeVisible();
  await tapOrClick(page, page.getByRole("button", { name: "Close issue" }));
  await expect(page.getByText("closed this as completed")).toBeVisible();
  await expectAccessible(page);
  await tapOrClick(page, page.getByRole("link", { name: "Edit", exact: true }));
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
