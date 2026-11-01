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

/** Taps on touch projects and clicks otherwise. */
export async function tapOrClick(_page: Page, target: Locator): Promise<void> {
  if (base.info().project.use.hasTouch) {
    await target.tap();
  } else {
    await target.click();
  }
}

interface Fixtures {
  /** Problems collected during the test; checked after it. */
  guard: { foreignRequests: string[]; cspViolations: string[]; stripped: string[]; allowStrip: boolean };
}

export const test = base.extend<Fixtures>({
  baseURL: async ({}, use) => {
    const url = process.env.SLATH_E2E_URL;
    if (!url) throw new Error("SLATH_E2E_URL is not set (global setup failed?)");
    await use(url);
  },
  guard: [
    async ({ page, baseURL }, use) => {
      const origin = new URL(baseURL!).origin;
      const guard = { foreignRequests: [] as string[], cspViolations: [] as string[], stripped: [] as string[], allowStrip: false };
      page.on("request", (req) => {
        const url = req.url();
        if (url.startsWith("data:") || url.startsWith("blob:") || url === "about:blank") return;
        if (new URL(url).origin !== origin) guard.foreignRequests.push(url);
      });
      page.on("console", (msg) => {
        const text = msg.text();
        if (/Content[- ]Security[- ]Policy|Refused to (load|execute|apply)|trusted.?types/i.test(text)) {
          guard.cspViolations.push(text);
        }
      });
      await page.exposeFunction("__slathStrip", (desc: string) => guard.stripped.push(desc));
      await page.addInitScript(() => {
        document.addEventListener("securitypolicyviolation", (e) => {
          console.error(`Content Security Policy violation: ${e.violatedDirective} ${e.blockedURI}`);
        });
        document.addEventListener("htmx:security:strip", (e) => {
          const t = e.target as Element;
          (window as unknown as { __slathStrip(d: string): void }).__slathStrip(t.outerHTML.slice(0, 200));
        });
      });
      await use(guard);
      expect(guard.foreignRequests, "requests to other origins").toEqual([]);
      expect(guard.cspViolations, "CSP violations").toEqual([]);
      if (!guard.allowStrip) {
        expect(guard.stripped, "htmx:security:strip (unstamped htmx element)").toEqual([]);
      }
    },
    { auto: true },
  ],
});

export { expect };
