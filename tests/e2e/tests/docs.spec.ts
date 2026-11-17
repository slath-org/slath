// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//
// The documentation in the binary (DOCS-01): served at /-/docs/ to
// everyone, from the instance alone, accessible in every theme and on a
// phone; the footer's Help leads there.
import { expect, expectAccessible, test } from "../fixtures";

const PAGES = ["/-/docs/", "/-/docs/user/merge-requests.html", "/-/docs/admin/install.html", "/-/docs/api/index.html", "/-/docs/api/merge-requests.html"];

test("conf_docs_01_pages_are_local_and_accessible", async ({ page, baseURL }) => {
  const origin = new URL(baseURL!).origin;
  const outside: string[] = [];
  page.on("request", (r) => {
    if (new URL(r.url()).origin !== origin) outside.push(r.url());
  });
  for (const path of PAGES) {
    const res = await page.goto(path);
    expect(res?.status(), path).toBe(200);
    await expect(page.locator("main h1")).toBeVisible();
    await expect(page.getByRole("navigation", { name: "Documentation" })).toBeAttached();
    await expectAccessible(page);
  }
  expect(outside).toEqual([]);
});

test("conf_docs_01_help_leads_to_the_local_documentation", async ({ page }) => {
  await page.goto("/-/login");
  await page.getByRole("link", { name: "Help", exact: true }).click();
  await expect(page).toHaveURL(/\/-\/docs\/$/);
  await expect(page.getByRole("heading", { level: 1 })).toHaveText("Slath documentation");
  await expect(page.getByText(/the version running here/)).toBeVisible();
  // Links between pages work, anchors included.
  await page.getByRole("link", { name: "Forks", exact: true }).first().click();
  await expect(page.getByRole("heading", { level: 1 })).toHaveText("Forks");
  await expect(page.locator("#the-branch-mrn-in-the-original")).toBeAttached();
});
