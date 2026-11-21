// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//
// The pages the appearance specs visit: the design catalog, one page per
// page group, and every page built from the components, with the steps
// that open each one.
import { type Page } from "@playwright/test";
import { USERS, expect, signIn, tapOrClick, test } from "../fixtures";

export type Account = "anon" | "alice" | "admin";

/** The design catalog, which only debug builds serve (see playwright.config.ts). */
export const CATALOG = process.env.SLATH_E2E_CATALOG === "0" ? [] : ["", "/foundations", "/frame", "/layout", "/messages", "/forms", "/disclosure", "/lists", "/people", "/controls", "/review", "/code"].map(
  (p) => `/-/design${p}`,
);

/** One page per page group, with the account that sees it. */
export const GROUPS: { path: string; who: "anon" | "alice" }[] = [
  { path: "/alice/demo", who: "alice" },
  { path: "/alice/demo/issues", who: "alice" },
  { path: "/alice/demo/merge-requests/1", who: "alice" },
  { path: "/-/settings/profile", who: "alice" },
  { path: "/-/login", who: "anon" },
  { path: "/-/no-such-page", who: "anon" },
];

/** The pages of the groups built from the components, signed in as alice. */
export const MIGRATED = [
  "/alice/demo",
  "/alice/demo/tree/main/src",
  "/alice/demo/blob/main/src/main.rs",
  "/alice/demo/blame/main/src/main.rs",
  "/alice/demo/commits",
  "/alice/demo/commit/", // the newest commit, opened from the list
  "/alice/demo/branches",
  "/alice/demo/tags",
  "/alice/demo/forks",
  "/alice/demo/fork",
  "/alice/demo/sync?branch=main",
  "/-/new",
  "/-/import",
  "/-/settings/emails",
  "/alice/demo/issues",
  "/alice/demo/issues?state=closed&labels=no-such-label",
  "/alice/demo/issues/", // a new issue
  "/alice/demo/issues/new",
  "/alice/demo/labels",
  "/alice/demo/milestones",
  "/-/search?q=demo",
  "/-/search?q=demo&type=repos",
  "/-/search?q=nothing-matches-this",
  "/alice/demo/merge-requests",
  "/alice/demo/merge-requests?state=all&sort=updated",
  "/-/review-requests",
  "/alice/demo/compare?source=review",
  "/alice/demo/merge-requests/1",
  "/alice/demo/merge-requests/1?show=unresolved",
  "/alice/demo/merge-requests/1/changes",
  "/alice/demo/merge-requests/1/files",
  "/-/settings/profile",
  "/-/settings/notifications",
  "/-/settings/appearance",
  "/-/settings/security",
  "/-/settings/sessions",
  "/-/settings/keys",
  "/-/settings/tokens",
  "/-/settings/bots",
  "/alice/demo/settings/general",
  "/alice/demo/settings/access",
  "/alice/demo/settings/keys",
  "/alice/demo/settings/protections",
  "/alice/demo/settings/webhooks",
  "/alice/demo/settings/danger",
  "/-/orgs/new",
  // Pages of an organization the test creates: "org:" and the path in it.
  "org:/-/settings",
  "org:/-/members",
  "org:/-/teams",
  "org:/-/teams/core",
  "org:/-/bots",
  "org:/-/webhooks",
  "org:bot",
  "org:",
  // Account, sign-in, and onboarding; "anon:" pages signed out.
  "/",
  "/alice",
  "/-/notifications",
  "/-/notifications?all=true",
  "/-/sudo?next=/-/settings/security",
  "anon:/-/login",
  // Errors: not found signed in and out, sign-in required.
  "/-/no-such-page",
  "anon:/-/no-such-page",
  "anon:/-/settings/profile",
  // Administration, as the site administrator.
  "/-/admin/users",
  "/-/admin/invites",
  "/-/admin/orgs",
  "/-/admin/bots",
  "/-/admin/repos",
  "/-/admin/audit",
  "/-/admin/upgrades",
  "/-/admin/health",
  "/-/admin/appearance",
];

/**
 * Signs in as the page needs and opens one of the MIGRATED pages. Some are
 * made first: "/commit/" is the newest commit, "/issues/" a new issue, and
 * "org:" pages belong to an organization created for the test, named
 * from `salt`, so specs don't collide. Signs in only if `signedIn` isn't
 * already the account the page needs, and returns that account.
 */
export async function openPage(page: Page, path: string, salt: string, signedIn?: Account): Promise<Account> {
  const who: Account = path.startsWith("anon:") ? "anon" : path.startsWith("/-/admin/") ? "admin" : "alice";
  if (who !== signedIn) {
    await page.context().clearCookies();
    if (who !== "anon") await signIn(page, USERS[who]);
  }
  if (path.endsWith("/commit/")) {
    await page.goto("/alice/demo/commits");
    await page.locator(".item-title").first().click();
    await expect(page).toHaveURL(/\/commit\/[0-9a-f]{40}$/);
  } else if (path.endsWith("/issues/")) {
    await page.goto("/alice/demo/issues/new");
    await page.getByLabel("Title").fill(`${salt} ${test.info().project.name}`);
    await tapOrClick(page, page.getByRole("button", { name: "Open issue" }));
    await expect(page).toHaveURL(/\/issues\/[0-9]+$/);
  } else if (path.startsWith("org:")) {
    const rest = path.slice(4);
    const tag = `${salt}-${test.info().project.name}-${rest}`;
    let h = 0;
    for (const ch of tag) h = (h * 31 + ch.charCodeAt(0)) >>> 0;
    const org = `w${h.toString(36)}`;
    await page.goto("/-/orgs/new");
    await page.getByLabel("Name", { exact: true }).fill(org);
    await tapOrClick(page, page.getByRole("button", { name: "Create organization" }));
    await expect(page.getByRole("heading", { level: 1 })).toHaveText(org);
    if (rest === "/-/teams/core") {
      await page.goto(`/${org}/-/teams`);
      await page.getByLabel("Name", { exact: true }).fill("core");
      await tapOrClick(page, page.getByRole("button", { name: "Create team" }));
      await expect(page.getByRole("status").filter({ hasText: "Team created." })).toBeVisible();
    }
    if (rest === "bot") {
      await page.goto(`/${org}/-/bots`);
      await page.getByLabel("Username").fill(`${org}-ci`);
      await tapOrClick(page, page.getByRole("button", { name: "Create bot" }));
      await expect(page.getByRole("status").filter({ hasText: "Bot created." })).toBeVisible();
    } else await page.goto(`/${org}${rest}`);
  } else await page.goto(path.replace(/^anon:/, ""));
  return who;
}
