// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//
// Moving through a stack of five changes on the change pages: with the
// keyboard alone (j and k between changes, [ and ] between versions, only
// while focus is in the change view) and with taps or clicks on the same
// buttons, in every browser cell. The e2e server seeds `stack/<project>`.
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

const stack = (page: Page) => page.getByRole("complementary", { name: "Stack of this merge request" });

/** The shown change's subject, as the stack marks it. */
async function shown(page: Page): Promise<string> {
  return (await stack(page).locator(".stack-item[aria-current=true] strong").innerText()).trim();
}

/** Keyboard alone: Tab to the step buttons, then j, k, and Enter. */
async function byKeyboard(page: Page, url: string): Promise<void> {
  await page.goto(`${url}/commits`);
  const first = await page.locator("section[aria-labelledby=commits-heading] .change-id").first().getAttribute("href");
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
  await page.goto(`${url}/commits`);
  await tapOrClick(page, page.locator("section[aria-labelledby=commits-heading] .change-id").first());
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
