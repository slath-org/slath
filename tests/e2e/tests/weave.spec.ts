// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//
// The Weave rules, measured in the browser:
// - on every catalog page, strictly: gaps inside the layout components are
//   the relationship distances, headings have more space above than below,
//   every disclosure shows an indicator that turns, no heading level skips;
// - on one page per page group, the structural rules (indicators, heading
//   levels) against weave-baseline.json, which lists the violations of
//   pages not yet built from the components. The baseline only shrinks: a
//   new violation fails, and so does a listed one that's gone;
// - on the pages of the groups built from the components, every rule,
//   strictly, as on the catalog.
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { type Page } from "@playwright/test";
import { USERS, expect, signIn, tapOrClick, test } from "../fixtures";

const CATALOG = ["", "/foundations", "/frame", "/layout", "/messages", "/forms", "/disclosure", "/lists", "/people", "/controls", "/review", "/code"].map(
  (p) => `/-/design${p}`,
);

/** One page per page group, with the account that sees it. */
const GROUPS: { path: string; who: "anon" | "alice" }[] = [
  { path: "/alice/demo", who: "alice" },
  { path: "/alice/demo/issues", who: "alice" },
  { path: "/alice/demo/merge-requests/1", who: "alice" },
  { path: "/-/settings/profile", who: "alice" },
  { path: "/-/login", who: "anon" },
  { path: "/-/no-such-page", who: "anon" },
];

/** The pages of the groups built from the components, signed in as alice. */
const MIGRATED = [
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
  "/alice/demo/merge-requests/1/commits",
  "/alice/demo/merge-requests/1/changes",
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
  // Administration, as the site administrator.
  "/-/admin/users",
  "/-/admin/invites",
  "/-/admin/orgs",
  "/-/admin/bots",
  "/-/admin/repos",
  "/-/admin/audit",
  "/-/admin/upgrades",
  "/-/admin/health",
];

const baseline: Record<string, string[]> = JSON.parse(
  readFileSync(resolve(import.meta.dirname, "../weave-baseline.json"), "utf8"),
);

/** Structural rules: every disclosure shows an indicator; no heading skips. */
async function structure(page: Page): Promise<string[]> {
  return page.evaluate(() => {
    const out: string[] = [];
    for (const d of document.querySelectorAll<HTMLDetailsElement>("main details")) {
      const s = d.querySelector(":scope > summary");
      if (!s) continue;
      const name = d.className.split(" ")[0] || "unnamed";
      if (d.classList.contains("reply-box")) continue;
      const chevron = s.querySelector(".disclosure-indicator, .disclosure, svg:has(use[href$='#chevron-down'])");
      const cs = getComputedStyle(s);
      const native = cs.display === "list-item" && cs.listStyleType !== "none";
      if (native) out.push(`details-native-marker ${name}`);
      else if (!chevron) out.push(`details-no-indicator ${name}`);
    }
    let prev = 0;
    for (const h of document.querySelectorAll("h1, h2, h3, h4, h5, h6")) {
      const level = Number(h.tagName[1]);
      if (prev && level > prev + 1) out.push(`heading-skip h${prev}->h${level}`);
      prev = level;
    }
    return [...new Set(out)].sort();
  });
}

/** Spacing rules for pages built from the components. */
async function spacing(page: Page): Promise<string[]> {
  return page.evaluate(() => {
    const out: string[] = [];
    const root = getComputedStyle(document.documentElement);
    const px = (name: string) => parseFloat(root.getPropertyValue(name)) * parseFloat(root.fontSize);
    const allowed = new Set(
      ["--gap-inside", "--gap-inline", "--gap-items", "--gap-groups", "--gap-sections", "--gap-below-h2", "--gap-below-h3"].map((t) => Math.round(px(t))),
    );
    allowed.add(0);
    const visible = (el: Element) => {
      const r = el.getBoundingClientRect();
      return r.width > 0 && r.height > 0 && getComputedStyle(el).visibility !== "hidden";
    };
    const containers = document.querySelectorAll(
      "main :is(.stack, .stack-inside, .stack-groups, .stack-sections, .page, .section, .subsection, .frame, .form-layout, .form-layout-wide)",
    );
    for (const c of containers) {
      const kids = [...c.children].filter((k) => visible(k) && getComputedStyle(k).position !== "absolute");
      for (let i = 1; i < kids.length; i++) {
        const a = kids[i - 1].getBoundingClientRect();
        const b = kids[i].getBoundingClientRect();
        if (b.top < a.bottom - 1) continue; // side by side
        const g = Math.round(b.top - a.bottom);
        if (!allowed.has(g)) out.push(`gap ${g}px in ${c.className.split(" ")[0]} before ${kids[i].tagName.toLowerCase()}.${kids[i].className.split(" ")[0]}`);
      }
    }
    // A heading has more space above it than below it.
    for (const h of document.querySelectorAll("main :is(h2, h3)")) {
      if (!visible(h)) continue;
      let prev = h.previousElementSibling;
      let host: Element | null = h.parentElement;
      while (!prev && host && host.tagName !== "MAIN") {
        // The first heading of a frame or a catalog example has the
        // frame's padding above it instead.
        if (host.matches(".frame, .catalog-canvas, .readiness-text, .section-head, .markdown")) break;
        prev = host.previousElementSibling;
        host = host.parentElement;
      }
      const next = h.nextElementSibling ?? (h.parentElement?.matches(".section-head") ? h.parentElement.nextElementSibling : null);
      if (!prev || !next) continue;
      const r = h.getBoundingClientRect();
      const above = Math.round(r.top - prev.getBoundingClientRect().bottom);
      const below = Math.round(next.getBoundingClientRect().top - r.bottom);
      if (above >= 0 && above <= below) out.push(`heading "${h.textContent?.trim().slice(0, 30)}" ${above}px above, ${below}px below`);
    }
    return [...new Set(out)].sort();
  });
}

for (const path of CATALOG) {
  test(`conf_design_03_catalog_follows_the_rules ${path}`, async ({ page }) => {
    await page.goto(path);
    expect(await structure(page)).toEqual([]);
    expect(await spacing(page)).toEqual([]);
    for (const density of ["compact"]) {
      await page.evaluate((d) => {
        document.documentElement.dataset.density = d;
      }, density);
      expect(await spacing(page), density).toEqual([]);
    }
  });
}

test("conf_design_03_disclosure_indicators_turn_when_open", async ({ page }) => {
  for (const path of ["/-/design/disclosure", "/-/design/review"]) {
    await page.goto(path);
    const summaries = page.locator("main details:not(.reply-box) > summary");
    for (const s of await summaries.all()) {
      if (!(await s.isVisible())) continue;
      const chevron = s.locator(".disclosure-indicator svg, .disclosure svg, svg:has(use[href$='#chevron-down'])").last();
      const before = await chevron.evaluate((e) => getComputedStyle(e).transform);
      await tapOrClick(page, s);
      await expect.poll(() => chevron.evaluate((e) => getComputedStyle(e).transform)).not.toBe(before);
      await page.keyboard.press("Escape");
      if (await s.evaluate((e) => (e.parentElement as HTMLDetailsElement).open)) await tapOrClick(page, s);
    }
  }
});

for (const { path, who } of GROUPS) {
  test(`conf_design_03_page_groups_shrink_their_baseline ${path}`, async ({ page }) => {
    if (who === "alice") await signIn(page, USERS.alice);
    await page.goto(path);
    const found = await structure(page);
    expect(found, `${path}: new violations, or fixed ones still in tests/e2e/weave-baseline.json`).toEqual(
      [...(baseline[path] ?? [])].sort(),
    );
  });
}

for (const path of MIGRATED) {
  test(`conf_design_03_migrated_pages_follow_the_rules ${path}`, async ({ page }) => {
    if (!path.startsWith("anon:")) await signIn(page, path.startsWith("/-/admin/") ? USERS.admin : USERS.alice);
    if (path.endsWith("/commit/")) {
      await page.goto("/alice/demo/commits");
      await page.locator(".item-title").first().click();
      await expect(page).toHaveURL(/\/commit\/[0-9a-f]{40}$/);
    } else if (path.endsWith("/issues/")) {
      await page.goto("/alice/demo/issues/new");
      await page.getByLabel("Title").fill(`Weave ${test.info().project.name}`);
      await tapOrClick(page, page.getByRole("button", { name: "Open issue" }));
      await expect(page).toHaveURL(/\/issues\/[0-9]+$/);
    } else if (path.startsWith("org:")) {
      const rest = path.slice(4);
      const tag = `${test.info().project.name}-${rest}`;
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
    expect(await structure(page)).toEqual([]);
    expect(await spacing(page)).toEqual([]);
    await page.evaluate(() => {
      document.documentElement.dataset.density = "compact";
    });
    expect(await spacing(page), "compact").toEqual([]);
  });
}

test("conf_design_03_checks_catch_violations", async ({ page }) => {
  await page.goto("/-/design/layout");
  await page.evaluate(() => {
    const main = document.querySelector("main")!;
    const d = document.createElement("details");
    const summary = document.createElement("summary");
    summary.textContent = "Bare marker";
    d.append(summary);
    main.append(d);
    const h = document.createElement("h4");
    h.textContent = "Skipped";
    main.querySelector("h1")!.after(h);
    const stack = document.querySelector("main .stack")!;
    const odd = document.createElement("div");
    odd.className = "catalog-block";
    odd.textContent = "Own margin";
    odd.style.marginBlockStart = "5px";
    stack.append(odd);
  });
  const found = await structure(page);
  expect(found.some((v) => v.startsWith("details-native-marker") || v.startsWith("details-no-indicator"))).toBe(true);
  expect(found).toContain("heading-skip h1->h4");
  expect((await spacing(page)).some((v) => v.startsWith("gap 21px"))).toBe(true);
});
