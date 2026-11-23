// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//
// Linked merge requests on a merge request's page: link another one, move
// it up, and merge both with the group's merge box, by taps or clicks and
// with the keyboard, in every browser cell. The e2e server seeds
// `linked/<project>/lib` and `linked/<project>/app`.
import type { Page } from "@playwright/test";
import { USERS, expect, expectAccessible, signIn, tapOrClick, test } from "../../fixtures";

/** Opens a merge request from `branch`; its number. */
async function open(page: Page, branch: string, title: string): Promise<number> {
  await page.goto(`/alice/demo/compare?source=${encodeURIComponent(branch)}`);
  await page.getByLabel("Title").fill(title);
  await tapOrClick(page, page.getByRole("button", { name: "Create merge request" }));
  await expect(page.getByRole("heading", { level: 1 })).toContainText(title);
  return Number(new URL(page.url()).pathname.split("/").pop());
}

const group = (page: Page) => page.getByRole("region", { name: "Linked merge requests" });

/** The members' references, in order. */
async function order(page: Page): Promise<string[]> {
  const items = group(page).locator(".stack-item");
  return (await items.locator("a, strong").allInnerTexts()).map((t) => t.trim());
}

test("conf_agent_07_link_move_and_merge_a_group", async ({ page }, info) => {
  const project = info.project.name;
  await signIn(page, USERS.alice);
  const lib = await open(page, `linked/${project}/lib`, `Lib for ${project}`);
  const app = await open(page, `linked/${project}/app`, `App for ${project}`);
  // Not linked yet: its own merge form, and a way to link.
  await expect(group(page)).toContainText("None.");
  await expect(page.locator("#merge-form")).toHaveCount(1);
  await tapOrClick(page, group(page).getByText("Link a merge request"));
  await group(page).getByLabel("Merge request").fill(`alice/demo!${lib}`);
  await tapOrClick(page, group(page).getByRole("button", { name: "Link", exact: true }));
  await expect(page.getByText("Linked. The group merges together, in its order.")).toBeVisible();
  expect(await order(page)).toEqual([`alice/demo!${app}`, `alice/demo!${lib}`]);
  await expect(group(page).locator(".stack-item[aria-current=true]")).toContainText(`alice/demo!${app}`);
  // The merge request's own merge form gives way to the group's.
  await expect(page.locator("#merge-form")).toHaveCount(0);
  await expectAccessible(page);
  // Move the library first.
  await tapOrClick(page, group(page).getByRole("button", { name: `Move alice/demo!${lib} up` }));
  await expect(page.getByText("Order saved.")).toBeVisible();
  expect(await order(page)).toEqual([`alice/demo!${lib}`, `alice/demo!${app}`]);
  await expect(group(page).getByRole("button", { name: `Move alice/demo!${lib} up` })).toHaveCount(0);
  // Both are ready: merge them in order.
  const box = page.getByRole("region", { name: "Merge the group" });
  await expect(box).toContainText(`alice/demo!${lib}: Ready to merge`);
  await expect(box).toContainText(`alice/demo!${app}: Ready to merge`);
  await tapOrClick(page, box.getByRole("button", { name: "Merge 2 merge requests in order" }));
  await expect(page.getByText("The group merged.")).toBeVisible();
  await expect(group(page)).toContainText("Merged: every merge request landed.");
  await expect(page.getByRole("region", { name: "Merge the group" })).toHaveCount(0);
  await expectAccessible(page);
  // The other member's page shows the same group, from its side.
  await page.goto(`/alice/demo/merge-requests/${lib}`);
  await expect(group(page).locator(".stack-item[aria-current=true]")).toContainText(`alice/demo!${lib}`);
  await expect(group(page)).toContainText("Merged: every merge request landed.");
});

test("conf_agent_07_link_and_unlink_with_the_keyboard", async ({ page }, info) => {
  const project = info.project.name;
  await signIn(page, USERS.alice);
  const one = await open(page, `linked/${project}/keys-lib`, `Keyboard lib for ${project}`);
  await open(page, `linked/${project}/keys-app`, `Keyboard app for ${project}`);
  const summary = group(page).getByText("Link a merge request");
  await summary.focus();
  await page.keyboard.press("Enter");
  const field = group(page).getByLabel("Merge request");
  await field.focus();
  await page.keyboard.type(`alice/demo!${one}`);
  await page.keyboard.press("Enter");
  await expect(page.getByText("Linked. The group merges together, in its order.")).toBeVisible();
  await expect(group(page).locator(".stack-item")).toHaveCount(2);
  // A reference that isn't a merge request is refused in place.
  await group(page).getByText("Link a merge request").focus();
  await page.keyboard.press("Enter");
  await group(page).getByLabel("Merge request").fill("alice/demo!99999");
  await group(page).getByRole("button", { name: "Link", exact: true }).focus();
  await page.keyboard.press("Enter");
  await expect(group(page)).toContainText("There's no such merge request, or you can't read it.");
  await expectAccessible(page);
  // Unlinking one of two dissolves the group.
  await group(page).getByRole("button", { name: "Unlink this one" }).focus();
  await page.keyboard.press("Enter");
  await expect(page.getByText("Unlinked: it merges on its own again.")).toBeVisible();
  await expect(group(page)).toContainText("None.");
  await expect(page.locator("#merge-form")).toHaveCount(1);
});
