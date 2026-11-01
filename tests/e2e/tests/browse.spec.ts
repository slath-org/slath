// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//
// The code browser on alice/demo (seeded by slath-e2e-server).
import { USERS, expect, expectAccessible, signIn, tapOrClick, test } from "../fixtures";

test("conf_a11y_01_code_browser_pages_accessible", async ({ page }) => {
  await signIn(page, USERS.alice);
  await page.goto("/alice/demo");
  await expect(page.getByRole("heading", { name: "README.md" })).toBeVisible();
  await expectAccessible(page);
  for (const path of [
    "/alice/demo/tree/main/src",
    "/alice/demo/blob/main/src/main.rs",
    "/alice/demo/commits",
    "/alice/demo/branches",
    "/alice/demo/tags",
  ]) {
    await page.goto(path);
    await expectAccessible(page);
  }
  await page.goto("/alice/demo/commits");
  await tapOrClick(page, page.getByRole("link", { name: "Greet the world" }));
  await expect(page.getByRole("heading", { level: 1 })).toHaveText("Greet the world");
  await expect(page.getByText("added").first()).toBeAttached();
  await expectAccessible(page);
});

test("conf_a11y_04_browse_files_by_touch_click_and_keyboard", async ({ page }) => {
  await signIn(page, USERS.alice);
  await page.goto("/alice/demo");
  await tapOrClick(page, page.getByRole("link", { name: "src" }));
  await expect(page.getByRole("navigation", { name: "Path" })).toContainText("src");
  await tapOrClick(page, page.getByRole("link", { name: "main.rs" }));
  await expect(page.locator("#L2")).toContainText("println!");
  // Keyboard: back to the root via the breadcrumb.
  await page.getByRole("navigation", { name: "Path" }).getByRole("link", { name: "demo" }).focus();
  await page.keyboard.press("Enter");
  await expect(page.getByRole("heading", { name: "README.md" })).toBeVisible();
});

test("conf_sec_04_readme_html_stays_inert", async ({ page }) => {
  let logoutRequests = 0;
  page.on("request", (r) => {
    if (r.url().includes("/-/logout")) logoutRequests++;
  });
  await signIn(page, USERS.alice);
  await page.goto("/alice/demo");
  await expect(page.locator(".markdown")).toHaveAttribute("hx-ignore", "");
  await page.waitForTimeout(300);
  expect(logoutRequests).toBe(0);
  await expect(page.locator(".markdown [hx-get]")).toHaveCount(0);
  // Still signed in: the injected element didn't fire.
  await expect(page.getByRole("navigation", { name: "Main" }).getByRole("link", { name: "alice", exact: true })).toBeVisible();
});
