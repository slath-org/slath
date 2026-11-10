// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//
// Shared fixtures for every browser test:
//  - baseURL from the launched server;
//  - a network recorder that fails the test on any request to another origin;
//  - failures on CSP violations and on htmx:security:strip events;
//  - expectAccessible(page): axe-core with the WCAG 2.0-2.2 A/AA rules;
//  - tapOrClick(locator): tap on touch projects, click otherwise. Tests never
//    hover, so nothing can depend on hover.
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import AxeBuilder from "@axe-core/playwright";
import { test as base, expect, type Locator, type Page } from "@playwright/test";

interface Exception {
  rule: string;
  selector?: string;
  reason: string;
  issue: string;
}

const exceptions: Exception[] = JSON.parse(
  readFileSync(resolve(import.meta.dirname, "a11y-exceptions.json"), "utf8"),
).exceptions;

export const WCAG_TAGS = ["wcag2a", "wcag2aa", "wcag21a", "wcag21aa", "wcag22aa"];

/** Runs axe on the page and fails on any violation not listed in a11y-exceptions.json. */
export async function expectAccessible(page: Page): Promise<void> {
  const results = await new AxeBuilder({ page }).withTags(WCAG_TAGS).analyze();
  const violations = results.violations
    .map((v) => ({
      ...v,
      nodes: v.nodes.filter(
        (n) => !exceptions.some((e) => e.rule === v.id && (!e.selector || n.target.join(" ") === e.selector)),
      ),
    }))
    .filter((v) => v.nodes.length > 0);
  expect(
    violations.map((v) => `${v.id}: ${v.help} (${v.nodes.map((n) => n.target.join(" ")).join(", ")})`),
  ).toEqual([]);
}

/**
 * The side menu named `name` (Repository, Settings, Administration, …),
 * opened first where it's a closed drawer (narrow screens).
 */
export async function sideMenu(page: Page, name: string): Promise<Locator> {
  const menu = page.getByRole("navigation", { name, exact: true });
  if (!(await menu.isVisible())) {
    await tapOrClick(page, page.getByRole("button", { name: "Side menu" }));
    await expect(menu).toBeVisible();
  }
  return menu;
}

/** Taps on touch projects and clicks otherwise. */
export async function tapOrClick(_page: Page, target: Locator): Promise<void> {
  if (base.info().project.use.hasTouch) {
    await target.tap();
  } else {
    await target.click();
  }
}

/**
 * The editing surface of a Markdown editor whose label starts with `label`
 * (the editor names it "<label> (rich text)" or "<label> (Markdown source)").
 */
export function editor(page: Page, label: string): Locator {
  return page.getByRole("textbox", { name: new RegExp(`^${label} \\(`) });
}

/**
 * Makes the editor on the page use rich text. The mode is stored per
 * account, and tests running in parallel share the account.
 */
export async function richText(page: Page): Promise<void> {
  await expect(page.locator("slath-editor[data-state=ready]").first()).toBeVisible();
  const toRich = page.getByRole("button", { name: "Switch to rich text" }).first();
  if (await toRich.isVisible()) await toRich.click();
  await expect(page.getByRole("button", { name: "Switch to Markdown source" }).first()).toBeVisible();
}

/** Types into a Markdown editor (works for both modes in every browser). */
export async function write(page: Page, label: string, text: string): Promise<void> {
  const box = editor(page, label);
  await tapOrClick(page, box);
  await page.keyboard.type(text);
}

/** Accounts created by slath-e2e-server. */
export const USERS = {
  admin: { name: "sysadmin", password: "e2e admin password" },
  alice: { name: "alice", password: "slath test password" },
} as const;

/** Signs in through the login form. */
export async function signIn(page: Page, user: { name: string; password: string }): Promise<void> {
  await page.goto("/-/login");
  await page.getByLabel("Username").fill(user.name);
  await page.getByLabel("Password").fill(user.password);
  await page.getByRole("button", { name: "Sign in", exact: true }).click();
  await expect(page.getByRole("navigation", { name: "Main" }).getByRole("link", { name: user.name, exact: true })).toBeVisible();
}

interface Fixtures {
  /** Problems collected during the test; checked after it. */
  guard: { foreignRequests: string[]; cspViolations: string[]; stripped: string[]; allowStrip: boolean };
}

export const test: ReturnType<typeof base.extend<Fixtures>> = base.extend<Fixtures>({
  baseURL: async ({}, use) => {
    const url = process.env.SLATH_E2E_URL;
    if (!url) throw new Error("SLATH_E2E_URL is not set (global setup failed?)");
    await use(url);
  },
  guard: [
    async ({ page, baseURL }, use) => {
      const origin = new URL(baseURL!).origin;
      const guard = { foreignRequests: [] as string[], cspViolations: [] as string[], stripped: [] as string[], allowStrip: false };
      let sawSelect = false;
      page.on("request", (req) => {
        const url = req.url();
        if (url.startsWith("data:") || url.startsWith("blob:") || url === "about:blank") return;
        if (new URL(url).origin !== origin) guard.foreignRequests.push(url);
      });
      page.on("console", (msg) => {
        const text = msg.text();
        if (text === "slath-e2e:has-select") {
          sawSelect = true;
          return;
        }
        if (/Content[- ]Security[- ]Policy|Refused to (load|execute|apply)|trusted.?types/i.test(text)) {
          guard.cspViolations.push(text);
        }
      });
      await page.exposeFunction("__slathStrip", (desc: string) => guard.stripped.push(desc));
      await page.addInitScript(() => {
        document.addEventListener("DOMContentLoaded", () => {
          if (document.querySelector("select")) console.log("slath-e2e:has-select");
        });
        document.addEventListener("securitypolicyviolation", (e) => {
          console.error(`Content Security Policy violation: ${e.violatedDirective} ${e.blockedURI}`);
        });
        document.addEventListener("htmx:security:strip", (e) => {
          const t = e.target as Element;
          (window as unknown as { __slathStrip(d: string): void }).__slathStrip(t.outerHTML.slice(0, 200));
        });
      });
      await use(guard);
      // WebKit's built-in <select> rendering reports an inline-style CSP
      // violation of its own (reproduced with a bare <select> under
      // style-src 'self'). It isn't page content, so on WebKit pages that
      // contained a <select> (on any page of the test) exactly these two
      // messages are ignored; Chromium
      // and Firefox still check those pages fully.
      let violations = guard.cspViolations;
      if (test.info().project.name.startsWith("webkit") && sawSelect) {
        violations = violations.filter(
          (v) =>
            !v.startsWith("Refused to apply a stylesheet because its hash, its nonce, or 'unsafe-inline'") &&
            v !== "Content Security Policy violation: style-src-attr inline",
        );
      }
      expect(guard.foreignRequests, "requests to other origins").toEqual([]);
      expect(violations, "CSP violations").toEqual([]);
      if (!guard.allowStrip) {
        expect(guard.stripped, "htmx:security:strip (unstamped htmx element)").toEqual([]);
      }
    },
    { auto: true },
  ],
});

export { expect };
