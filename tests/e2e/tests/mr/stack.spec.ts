// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//
// Moving through a stack of five changes on the change pages: with the
// keyboard alone (j and k between changes, [ and ] between versions, only
// while focus is in the change view) and with taps or clicks on the same
// buttons, in every browser cell. The e2e server seeds `stack/<project>`,
// "Outcomes fixture" for what each revision did to its changes, "jj
// conflict fixture" for conflicts recorded by jj, and `land/<project>` for
// landing part of a stack.
import type { Page } from "@playwright/test";
import { USERS, expect, expectAccessible, signIn, tapOrClick, test } from "../../fixtures";

/** Opens the merge request of the project's stack; its path. */
async function open(page: Page, project: string): Promise<string> {
  await page.goto(`/alice/demo/compare?source=${encodeURIComponent(`stack/${project}`)}`);
  await page.getByLabel("Title").fill(`Five steps in ${project}`);
  await tapOrClick(page, page.getByRole("button", { name: "Create merge request" }));
  await expect(page.getByRole("heading", { level: 1 })).toContainText(`Five steps in ${project}`);
  return new URL(page.url()).pathname;
}

const stack = (page: Page) => page.getByRole("region", { name: "Stack", exact: true });

/** The shown change's subject, as the stack marks it. */
async function shown(page: Page): Promise<string> {
  return (await stack(page).locator(".stack-item[aria-current=true] strong").innerText()).trim();
}

/** Keyboard alone: Tab to the step buttons, then j, k, and Enter. */
async function byKeyboard(page: Page, url: string): Promise<void> {
  await page.goto(`${url}/changes`);
  const first = await page.locator("section[aria-labelledby=changes-heading] a.item-title").first().getAttribute("href");
  await page.goto(first ?? "");
  await expect(stack(page).locator(".stack-item")).toHaveCount(5);
  expect(await shown(page)).toBe("Step 1");
  await expectAccessible(page);
  // Keys outside the change view do nothing.
  await page.locator("body").press("j");
  expect(await shown(page)).toBe("Step 1");
  // Tab to the first step button, then move by keys alone.
  const next = page.getByRole("link", { name: "Next change" });
  for (let i = 0; i < 80 && !(await next.evaluate((e) => e === document.activeElement)); i++) {
    await page.keyboard.press("Tab");
  }
  await expect(next).toBeFocused();
  for (const step of ["Step 2", "Step 3", "Step 4", "Step 5"]) {
    await page.keyboard.press("j");
    await expect(stack(page).locator(".stack-item[aria-current=true]")).toContainText(step);
  }
  // The top change has no next change; focus stays on a step button.
  await expect(page.getByRole("link", { name: "Next change" })).toHaveCount(0);
  await expect(page.locator("a[data-key]:focus")).toHaveCount(1);
  await page.keyboard.press("j");
  expect(await shown(page)).toBe("Step 5");
  await page.keyboard.press("k");
  await expect(stack(page).locator(".stack-item[aria-current=true]")).toContainText("Step 4");
  // One version only: no version steps.
  await expect(page.locator("a[data-key='[']")).toHaveCount(0);
  // Enter on a stack link works like a click.
  await stack(page).getByRole("link", { name: "Step 1" }).focus();
  await page.keyboard.press("Enter");
  await expect(stack(page).locator(".stack-item[aria-current=true]")).toContainText("Step 1");
  await expectAccessible(page);
}

/** Taps (touch cells) or clicks on the same buttons and the stack. */
async function byPointer(page: Page, url: string): Promise<void> {
  await page.goto(`${url}/changes`);
  await tapOrClick(page, page.locator("section[aria-labelledby=changes-heading] a.item-title").first());
  expect(await shown(page)).toBe("Step 1");
  for (const step of ["Step 2", "Step 3", "Step 4", "Step 5"]) {
    await tapOrClick(page, page.getByRole("link", { name: "Next change" }));
    await expect(stack(page).locator(".stack-item[aria-current=true]")).toContainText(step);
  }
  await tapOrClick(page, page.getByRole("link", { name: "Previous change" }));
  await expect(stack(page).locator(".stack-item[aria-current=true]")).toContainText("Step 4");
  await tapOrClick(page, stack(page).getByRole("link", { name: "Step 2" }));
  await expect(stack(page).locator(".stack-item[aria-current=true]")).toContainText("Step 2");
  // On narrow screens the stack follows the change; it's all reachable.
  await stack(page).scrollIntoViewIfNeeded();
  await expect(stack(page)).toBeInViewport();
  await expectAccessible(page);
}

test("conf_jj_04_moving_through_a_stack_of_five", async ({ page }, testInfo) => {
  // About twenty page loads in the mouse cells, which walk it both ways.
  test.setTimeout(120_000);
  await signIn(page, USERS.alice);
  const url = await open(page, testInfo.project.name);
  // Touch cells move by touch alone.
  if (!testInfo.project.use.hasTouch) await byKeyboard(page, url);
  await byPointer(page, url);
});

// "Outcomes fixture": in revision 3, alpha is unchanged, beta returned
// after revision 2 removed it, delta is new, and gamma was removed.
test("conf_jj_06_outcomes_on_the_changes_tab_and_change_pages", async ({ page }) => {
  await signIn(page, USERS.alice);
  await page.goto("/alice/demo/merge-requests");
  await tapOrClick(page, page.getByRole("link", { name: "Outcomes fixture" }));
  await expect(page.getByRole("heading", { level: 1 })).toContainText("Outcomes fixture");
  const url = new URL(page.url()).pathname;
  await page.goto(`${url}/changes`);
  const changes = page.locator("section[aria-labelledby=changes-heading]");
  await expect(changes.locator(".item-row").nth(0)).toContainText("Unchanged in revision 3: the same patch.");
  await expect(changes.locator(".item-row").nth(1)).toContainText(
    "Returned in revision 3, after it was removed in revision 2.",
  );
  await expect(changes.locator(".item-row").nth(2)).toContainText("New in revision 3.");
  const removed = page.locator("section[aria-labelledby=removed-heading]");
  await expect(removed).toContainText("removed in revision 3");
  await expectAccessible(page);
  // The removed change's page says when and why it left.
  await tapOrClick(page, removed.getByRole("link"));
  await expect(page.getByText("Removed in revision 3: abandoned, squashed into another change")).toBeVisible();
  await expectAccessible(page);
  // The returned change says so in the stack.
  await tapOrClick(page, stack(page).getByRole("link", { name: "Add beta" }));
  await expect(stack(page).locator(".stack-item[aria-current=true]")).toContainText("Returned in revision 3");
  await expectAccessible(page);
});

// "jj conflict fixture": jj made three changes, the second with a conflict
// that the third inherits.
test("conf_jj_07_conflicts_recorded_by_jj_on_change_pages", async ({ page }) => {
  await signIn(page, USERS.alice);
  await page.goto("/alice/demo/merge-requests");
  await tapOrClick(page, page.getByRole("link", { name: "jj conflict fixture" }));
  await expect(page.getByRole("heading", { level: 1 })).toContainText("jj conflict fixture");
  await expect(page.getByText("2 commits with a conflict recorded by jj.").first()).toBeVisible();
  const url = new URL(page.url()).pathname;
  await page.goto(`${url}/changes`);
  await tapOrClick(page, page.getByRole("link", { name: "Add top" }));
  // The frame, not the readiness card, which has the same name.
  const conflict = page.locator("#conflict");
  await expect(conflict).toContainText("can't merge until a push resolves it");
  await expect(stack(page).locator(".stack-item[aria-current=true]")).toContainText("Conflict");
  await expectAccessible(page);
  // The change above points at the lowest conflict, whose page shows how
  // to resolve it with either tool.
  await tapOrClick(page, conflict.getByRole("link"));
  await expect(stack(page).locator(".stack-item[aria-current=true]")).toContainText("Change shared");
  await expect(conflict).not.toContainText("below has a conflict too");
  await tapOrClick(page, conflict.getByLabel("jj", { exact: true }));
  await expect(conflict.locator("#conflict-jj")).toContainText("jj bookmark track jj-conflict@origin");
  await expect(conflict.locator("#conflict-git")).toBeHidden();
  await tapOrClick(page, conflict.getByLabel("Git", { exact: true }));
  await expect(conflict.locator("#conflict-git")).toContainText("git rebase -i origin/main");
  await expectAccessible(page);
});

// `land/<project>`: three changes; the first two land, the third stays.
test("conf_review_08_land_up_to_here_from_a_change_page", async ({ page }, info) => {
  const project = info.project.name;
  await signIn(page, USERS.alice);
  await page.goto(`/alice/demo/compare?source=${encodeURIComponent(`land/${project}`)}`);
  await page.getByLabel("Title").fill(`Three parts in ${project}`);
  await tapOrClick(page, page.getByRole("button", { name: "Create merge request" }));
  await expect(page.getByRole("heading", { level: 1 })).toContainText(`Three parts in ${project}`);
  const url = new URL(page.url()).pathname;
  // The top change lands only with the whole merge request.
  await page.goto(`${url}/changes`);
  await tapOrClick(page, page.getByRole("link", { name: "Part 3" }));
  await expect(stack(page).locator(".stack-item[aria-current=true]")).toContainText("Part 3");
  await expect(page.getByRole("region", { name: "Land up to here" })).toHaveCount(0);
  await tapOrClick(page, stack(page).getByRole("link", { name: "Part 2" }));
  const land = page.getByRole("region", { name: "Land up to here" });
  await expect(land).toContainText("Merges changes 1–2 of 3 into main");
  await expectAccessible(page);
  await land.getByLabel("Land with").selectOption("rebase");
  await tapOrClick(page, land.getByRole("button", { name: "Land 2 changes" }));
  await expect(page.getByText("Landed. The rest of the stack is a new revision on the new target.")).toBeVisible();
  await expect(page.locator("section[aria-labelledby=changes-heading] .item-row")).toHaveCount(1);
  await expect(page.locator("section[aria-labelledby=changes-heading]")).toContainText("Part 3");
  await expectAccessible(page);
  // The timeline says what landed.
  await page.goto(url);
  await expect(page.getByText(/landed changes \S+, \S+ into main \(rebase\); the rest is still open/)).toBeVisible();
});

test("conf_agent_15_comparing_versions_shows_how_the_description_changed", async ({ page }) => {
  await signIn(page, USERS.alice);
  await page.goto("/alice/demo/merge-requests");
  await tapOrClick(page, page.getByRole("link", { name: "Description fixture" }));
  await tapOrClick(page, page.getByRole("navigation", { name: "Merge request" }).getByRole("link", { name: "Changes" }));
  await tapOrClick(page, page.locator("section[aria-labelledby=changes-heading] .item-list > li a.item-title").first());
  await tapOrClick(page, page.getByRole("link", { name: "Compare with version 2" }));
  const section = page.getByRole("region", { name: "Description changed" });
  await expect(section).toContainText("Removed: Add notes");
  await expect(section).toContainText("Added: Add notes, reworded");
  await expect(section).toContainText("Kept as it is.");
  await expectAccessible(page);
});

test("conf_agent_15_a_brief_written_on_its_page_shows_on_the_change", async ({ page }, testInfo) => {
  await signIn(page, USERS.alice);
  await page.goto("/alice/demo/merge-requests");
  const p = testInfo.project.name;
  await page.goto(`/alice/demo/compare?source=${encodeURIComponent(`brief/${p}`)}`);
  const create = page.getByRole("button", { name: "Create merge request" });
  if (await create.isVisible()) {
    await tapOrClick(page, create);
  } else {
    // A retry: the merge request is open already.
    await tapOrClick(page, page.getByRole("note").getByRole("link"));
  }
  await expect(page.getByRole("heading", { level: 1 })).toContainText(`Add a briefed note for ${p}`);
  await tapOrClick(page, page.getByRole("navigation", { name: "Merge request" }).getByRole("link", { name: "Changes" }));
  await tapOrClick(page, page.locator("section[aria-labelledby=changes-heading] .item-list > li a.item-title").first());
  // A retried test finds its first brief and edits it.
  await tapOrClick(page, page.getByRole("link", { name: /^(Write a brief|Edit the brief)$/ }));
  await expect(page.getByRole("heading", { level: 1 })).toHaveText(/brief .*change [k-z]{8}$/);
  await expectAccessible(page);
  // Without an intent the form says what's missing and keeps the rest.
  await page.getByLabel("Intent").fill("");
  await page.getByLabel("Known gaps").fill(`Gaps seen in ${testInfo.project.name}`);
  await tapOrClick(page, page.getByRole("button", { name: "Save brief" }));
  await expect(page.getByRole("alert")).toContainText("Say what the change is for.");
  await expect(page.getByLabel("Known gaps")).toHaveValue(`Gaps seen in ${testInfo.project.name}`);
  await expectAccessible(page);
  const intent = `Reworded notes, checked in ${testInfo.project.name}`;
  await page.getByLabel("Intent").fill(intent);
  await tapOrClick(page, page.getByRole("button", { name: "Save brief" }));
  const brief = page.getByRole("region", { name: "Brief", exact: true });
  await expect(brief).toContainText("Author's claim");
  await expect(brief).toContainText(intent);
  await expect(brief.getByRole("link", { name: "Edit the brief" })).toBeVisible();
  await expectAccessible(page);
  // A comment on one field opens a form under it and starts a thread.
  await tapOrClick(page, brief.getByRole("link", { name: "Comment on the intent" }));
  const question = `Why reword, asked in ${testInfo.project.name}?`;
  await page.getByLabel("Comment on the intent").fill(question);
  await expectAccessible(page);
  await tapOrClick(page, page.getByRole("button", { name: "Add comment" }));
  await tapOrClick(page, brief.getByRole("link", { name: "Thread" }).last());
  const thread = page.locator(".timeline-thread").filter({ hasText: question });
  await expect(thread).toContainText(/Brief of change [k-z]{8}, version 1: Intent/);
  await expect(thread).toContainText(intent);
  await expectAccessible(page);
  // Closed again, so the fixture's open merge requests stay few.
  await tapOrClick(page, page.getByText("More actions"));
  await tapOrClick(page, page.getByRole("button", { name: "Close merge request" }));
  await expect(page.locator(".state-label")).toHaveText("Closed");
});
