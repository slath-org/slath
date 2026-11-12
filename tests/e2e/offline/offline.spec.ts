// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//
// Every page renders completely from the release image with outbound
// networking disabled: no request leaves the origin and none fails.
import { type Page, expect, test } from "@playwright/test";

const BASE = process.env.SLATH_OFFLINE_URL ?? "";
const PASSWORD = process.env.SLATH_OFFLINE_PASSWORD ?? "";

function watch(page: Page): string[] {
  const problems: string[] = [];
  const origin = new URL(BASE).origin;
  page.on("request", (r) => {
    const u = new URL(r.url());
    if (u.protocol !== "data:" && u.protocol !== "blob:" && u.origin !== origin) problems.push(`external request: ${r.url()}`);
  });
  page.on("requestfailed", (r) => problems.push(`failed: ${r.url()} ${r.failure()?.errorText ?? ""}`));
  page.on("console", (m) => {
    if (m.type() === "error" && /Content Security Policy|Refused to/.test(m.text())) problems.push(`console: ${m.text()}`);
  });
  return problems;
}

async function signIn(page: Page): Promise<void> {
  await page.goto("/-/login");
  await page.getByLabel("Username").fill("ada");
  await page.getByLabel("Password").fill(PASSWORD);
  await page.getByRole("button", { name: "Sign in" }).click();
  await expect(page.locator("a.nav-avatar[href=\"/ada\"]")).toBeVisible();
}

test("setup", async ({ page }) => {
  await signIn(page);
  await page.goto("/-/new");
  await page.getByLabel("Name", { exact: true }).fill("offline");
  await page.locator("#vis-public").check();
  await page.getByRole("button", { name: "Create repository" }).click();
  await expect(page.getByRole("heading", { level: 1 })).toContainText("offline");
  await page.goto("/-/orgs/new");
  await page.getByLabel("Name", { exact: true }).fill("acme");
  await page.getByRole("button", { name: "Create organization" }).click();
  await expect(page.getByRole("heading", { level: 1 })).toHaveText("acme");
});

test("pages", async ({ page }) => {
  const problems = watch(page);
  const anonymous = ["/", "/-/login", "/-/no-such-page", "/ada", "/acme", "/ada/offline"];
  for (const path of anonymous) {
    await page.goto(path, { waitUntil: "networkidle" });
  }
  await signIn(page);
  const pages = [
    "/",
    "/-/new",
    "/-/orgs/new",
    "/-/settings/profile",
    "/-/settings/security",
    "/-/settings/sessions",
    "/-/settings/keys",
    "/-/settings/tokens",
    "/-/admin/users",
    "/-/admin/orgs",
    "/-/admin/repos",
    "/-/admin/audit",
    "/-/admin/health",
    "/acme",
    "/acme/-/members",
    "/acme/-/settings",
    "/acme/-/audit",
    "/ada",
    "/ada/offline",
    "/ada/offline/tree/main/src",
    "/ada/offline/blob/main/src/main.rs",
    "/ada/offline/blob/main/README.md",
    "/ada/offline/commits/main",
    "/ada/offline/settings/general",
    "/ada/offline/settings/access",
    "/ada/offline/settings/danger",
  ];
  for (const path of pages) {
    const r = await page.goto(path, { waitUntil: "networkidle" });
    expect(r?.status(), path).toBeLessThan(400);
  }
  await page.goto("/ada/offline/commits/main");
  const commit = await page.locator("a[href*='/commit/']").first().getAttribute("href");
  expect(commit).toBeTruthy();
  const r = await page.goto(commit ?? "/", { waitUntil: "networkidle" });
  expect(r?.status()).toBeLessThan(400);
  expect(problems).toEqual([]);
});
