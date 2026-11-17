// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//
// The merge request UX items (spec: UX requirements) as scenarios, in
// every browser cell (mouse and touch, light and dark), each with a stack
// of two changes made with Git, one made with jj, and a mixed one (Git
// under jj). The UI states must be the same for all three apart from
// names and IDs (JJ-11). The e2e server seeds `ux/<project>/<kind>` and
// `ux/<project>/conflict`, and "Performance fixture" (50 files, 200
// comments).
import type { Page } from "@playwright/test";
import { USERS, expect, expectAccessible, signIn, tapOrClick, test } from "../../fixtures";

const KINDS = ["git", "jj", "mixed"] as const;

/** Text of `locator`s, with names, numbers, and IDs made neutral. */
async function texts(page: Page, selector: string, kind: string, project: string): Promise<string[]> {
  const all = await page.locator(selector).allInnerTexts();
  return all.map((t) =>
    t
      .replaceAll(`ux/${project}/${kind}`, "BRANCH")
      .replaceAll(`ux-${project}-${kind}`, "FILE")
      .replace(/![0-9]+/g, "!N")
      .replace(/\b[k-z]{8,32}\b/g, "CHANGE")
      .replace(/\b[0-9a-f]{7,40}\b/g, "OID")
      .replace(/just now|\d+ (?:minute|hour)s? ago/g, "TIME")
      .replace(/\s+/g, " ")
      .trim(),
  );
}

/** One stack through UX items 1, 2, 3, 5, 6, 8, and 10; its UI states. */
async function walk(page: Page, kind: string, project: string): Promise<Record<string, string[]>> {
  const state: Record<string, string[]> = {};
  const branch = `ux/${project}/${kind}`;
  // 1. The push hint's link: the prefilled compare page.
  await page.goto(`/alice/demo/compare?source=${encodeURIComponent(branch)}`);
  await expect(page.getByRole("heading", { level: 1 })).toHaveText("New merge request");
  // 2. Commits, the diff, and the form.
  await expect(page.getByRole("heading", { name: "2 commits" })).toBeVisible();
  await expect(page.getByRole("heading", { name: "2 changed files" })).toBeVisible();
  await expect(page.getByLabel("Create as draft")).not.toBeChecked();
  state.compare = await texts(page, "section[aria-labelledby=commits-heading] .item-list > li", kind, project);
  await expectAccessible(page);
  // 10. A title with code.
  await page.getByLabel("Title").fill("Add `alpha` and `beta`");
  await tapOrClick(page, page.getByRole("button", { name: "Create merge request" }));
  const heading = page.getByRole("heading", { level: 1 });
  await expect(heading).toContainText("Add alpha and beta");
  await expect(heading.locator("code")).toHaveText(["alpha", "beta"]);
  await expect(page).toHaveTitle(/Add `alpha` and `beta`/);
  const url = new URL(page.url()).pathname;
  // 3. The readiness card: one headline and its rows.
  const card = page.locator("section.readiness").first();
  await expect(card).toBeVisible();
  state.card = await texts(page, "section.readiness >> nth=0", kind, project);
  await expect(page.locator(".button-primary")).toHaveCount(1);
  // 8. The first revision in the timeline.
  await expect(page.locator("#revision-1")).toContainText("opened revision 1 with 2 commits");
  state.timeline = await texts(page, "#revision-1", kind, project);
  // 5. Checkout commands, one tool at a time, no tool badges.
  await tapOrClick(page, page.locator("summary", { hasText: "Check out" }));
  await expect(page.locator("#checkout-git")).toBeVisible();
  await expect(page.locator("#checkout-jj")).toBeHidden();
  await page.keyboard.press("Escape");
  await expect(page.locator(".badge", { hasText: /^(Git|jj)$/ })).toHaveCount(0);
  await expectAccessible(page);
  // The commits tab: one change per commit, in jj's format.
  await page.goto(`${url}/commits`);
  const rows = page.locator("section[aria-labelledby=commits-heading] .item-list > li");
  await expect(rows).toHaveCount(2);
  await expect(rows.locator(".change-id")).toHaveText([/^[k-z]{8}$/, /^[k-z]{8}$/]);
  state.commits = await texts(page, "section[aria-labelledby=commits-heading] .item-list > li", kind, project);
  // 6. Review: the file tree, viewed marks, threads, and the navigation.
  await page.goto(`${url}/changes`);
  const tree = page.getByRole("navigation", { name: "Changed files" });
  await expect(tree.getByRole("link")).toHaveCount(2);
  await expect(page.getByRole("link", { name: "Next unviewed file" })).toHaveAttribute("href", "#diff-0");
  await tapOrClick(page, page.locator("#diff-0").getByRole("button", { name: /^Mark viewed/ }));
  await expect(tree).toContainText("viewed");
  await expect(page.getByRole("link", { name: "Next unviewed file" })).toHaveAttribute("href", "#diff-1");
  await tapOrClick(page, page.locator("#diff-1 a[title='Comment on this line']").first());
  await page.getByLabel("Comment", { exact: true }).fill("Why beta?");
  await tapOrClick(page, page.getByRole("button", { name: "Comment now" }));
  await expect(page.locator(".diff-thread-row").filter({ hasText: "Why beta?" })).toBeVisible();
  await expect(tree).toContainText("1 thread, 1 unresolved");
  const next = page.getByRole("link", { name: "Next unresolved thread" });
  await tapOrClick(page, next);
  await expect(page.locator(".diff-thread-row[data-unresolved]").first()).toBeFocused();
  state.tree = await texts(page, "nav[aria-label='Changed files'] li", kind, project);
  await expectAccessible(page);
  // A pending comment: the conversation counts it.
  await tapOrClick(page, page.locator("#diff-1 a[title='Comment on this line']").first());
  await page.getByLabel("Comment", { exact: true }).fill("Later.");
  await tapOrClick(page, page.getByRole("button", { name: "Add to my review" }));
  await expect(page.locator(".diff-thread-row").filter({ hasText: "Later." })).toBeVisible();
  await page.goto(url);
  await expect(page.getByText("You have 1 pending comment")).toBeVisible();
  return state;
}

test("conf_jj_11_ux_items_are_the_same_for_git_jj_and_mixed_stacks", async ({ page }, testInfo) => {
  test.setTimeout(240_000);
  const project = testInfo.project.name;
  await signIn(page, USERS.alice);
  const states: Record<string, Record<string, string[]>> = {};
  for (const kind of KINDS) {
    states[kind] = await walk(page, kind, project);
  }
  for (const kind of ["jj", "mixed"]) {
    for (const key of Object.keys(states.git)) {
      expect(states[kind][key], `${kind}, ${key}`).toEqual(states.git[key]);
    }
  }
  // 10. The list shows the titles' code too.
  await page.goto("/alice/demo/merge-requests");
  const listed = page.getByRole("link", { name: "Add alpha and beta" });
  await expect(listed.first().locator("code").first()).toHaveText("alpha");
});

test("ux_mr_04_conflicts_list_files_and_commands_for_git_and_jj", async ({ page }, testInfo) => {
  const project = testInfo.project.name;
  await signIn(page, USERS.alice);
  await page.goto(`/alice/demo/compare?source=${encodeURIComponent(`ux/${project}/conflict`)}`);
  await page.getByLabel("Title").fill(`Conflict in ${project}`);
  await tapOrClick(page, page.getByRole("button", { name: "Create merge request" }));
  await expect(page.getByRole("heading", { level: 1 })).toContainText(`Conflict in ${project}`);
  const conflicts = page.locator("#conflicts");
  await expect(conflicts).toBeAttached();
  await tapOrClick(page, conflicts.locator("summary", { hasText: "Resolve the conflicts on your machine" }));
  await expect(page.locator("#conflicts-git")).toBeVisible();
  await expect(page.locator("#conflicts-git")).toContainText("git");
  await expect(page.locator("#conflicts-jj")).toBeHidden();
  await tapOrClick(page, page.getByRole("radio", { name: "jj" }).last());
  await expect(page.locator("#conflicts-jj")).toBeVisible();
  await expect(page.locator("#conflicts-jj")).toContainText("jj");
  await expect(page.locator("section.readiness").first()).toContainText(`ux-${project}-shared.md`);
  await expectAccessible(page);
});

test("ux_mr_09_conversation_render_budget_and_progressive_diff", async ({ page }, testInfo) => {
  // One measurement is enough; the budget holds for the server.
  test.skip(testInfo.project.name !== "chromium-light-desktop", "measured once");
  await signIn(page, USERS.alice);
  await page.goto("/alice/demo/merge-requests");
  await tapOrClick(page, page.getByRole("link", { name: "Performance fixture" }));
  await expect(page.getByText("Comment 199:")).toBeVisible();
  const url = new URL(page.url()).pathname;
  const budget = Number(process.env.SLATH_E2E_RENDER_BUDGET_MS ?? 300);
  const times: number[] = [];
  for (let i = 0; i < 20; i++) {
    const start = Date.now();
    const r = await page.request.get(url);
    expect(r.status()).toBe(200);
    await r.body();
    times.push(Date.now() - start);
  }
  times.sort((a, b) => a - b);
  const p95 = times[Math.ceil(times.length * 0.95) - 1];
  console.log(`conversation of 50 files and 200 comments: p95 ${p95} ms (${times.join(", ")})`);
  expect(p95, `p95 ${p95} ms`).toBeLessThan(budget);
  // The diff streams: the first files with the page, the rest as they scroll in.
  await page.goto(`${url}/changes`);
  await expect(page.locator(".diff-file")).toHaveCount(50);
  await expect(page.locator(".diff-file .diff-hunk")).toHaveCount(20);
  await page.locator("#diff-49").scrollIntoViewIfNeeded();
  await expect(page.locator("#diff-49 .diff-hunk")).toBeVisible();
});
