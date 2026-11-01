// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
import { USERS, expect, expectAccessible, signIn, tapOrClick, test } from "../fixtures";

test("conf_a11y_01_issue_pages_accessible", async ({ page }, testInfo) => {
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
