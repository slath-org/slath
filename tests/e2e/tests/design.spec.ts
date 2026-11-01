// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//
// Bundled fonts and icons on the design catalog.
import { expect, expectAccessible, test } from "../fixtures";

test("conf_design_01_fonts_load_without_layout_shift", async ({ page }) => {
  // Record layout shifts from the very first paint (Chromium reports them;
  // in other browsers the list stays empty and the font checks still run).
  await page.addInitScript(() => {
    (window as unknown as { __cls: number }).__cls = 0;
    try {
      new PerformanceObserver((list) => {
        for (const e of list.getEntries() as unknown as { value: number; hadRecentInput: boolean }[]) {
          if (!e.hadRecentInput) (window as unknown as { __cls: number }).__cls += e.value;
        }
      }).observe({ type: "layout-shift", buffered: true });
    } catch {
      // layout-shift entries are Chromium-only.
    }
  });
  await page.goto("/-/design");
  await page.evaluate(() => document.fonts.ready);
  await page.waitForTimeout(200);
  const cls = await page.evaluate(() => (window as unknown as { __cls: number }).__cls);
  expect(cls).toBeLessThan(0.01);
  const loaded = await page.evaluate(() =>
    [...document.fonts].filter((f) => f.status === "loaded").map((f) => f.family.replaceAll('"', "")),
  );
  expect(loaded).toContain("Source Sans 3");
  expect(loaded).toContain("JetBrains Mono");
  expect(await page.evaluate(() => document.fonts.check('15px "Source Sans 3"'))).toBe(true);
});

test("conf_design_01_code_renders_without_ligatures", async ({ page }) => {
  await page.goto("/-/design");
  for (const sel of ["#code-sample", "#inline-code"]) {
    const style = await page.locator(sel).evaluate((el) => {
      const s = getComputedStyle(el);
      return { lig: s.fontVariantLigatures, features: s.fontFeatureSettings, family: s.fontFamily };
    });
    expect(style.lig).toBe("none");
    expect(style.features).toContain('"liga" 0');
    expect(style.features).toContain('"calt" 0');
    expect(style.family.replaceAll('"', "").startsWith("JetBrains Mono")).toBe(true);
  }
  // Monospace cells: "->" is exactly two cells wide, like "ab".
  const widths = await page.evaluate(async () => {
    await document.fonts.ready;
    const measure = (t: string) => {
      const s = document.createElement("span");
      s.className = "code-view";
      s.style.display = "inline";
      s.style.padding = "0";
      s.style.border = "0";
      s.style.fontFamily = "var(--font-mono)";
      s.textContent = t;
      document.body.append(s);
      const w = s.getBoundingClientRect().width;
      s.remove();
      return w;
    };
    return { arrow: measure("->"), neq: measure("!="), fi: measure("fi"), ab: measure("ab") };
  });
  expect(widths.arrow).toBeCloseTo(widths.ab, 1);
  expect(widths.neq).toBeCloseTo(widths.ab, 1);
  expect(widths.fi).toBeCloseTo(widths.ab, 1);
});

test("conf_design_01_icons_come_from_the_instance_sprite", async ({ page, baseURL }) => {
  await page.goto("/-/design");
  const hrefs = await page.locator("svg.icon use").evaluateAll((uses) => uses.map((u) => u.getAttribute("href")));
  expect(hrefs.length).toBeGreaterThan(10);
  for (const h of hrefs) {
    expect(new URL(h!, baseURL).origin).toBe(new URL(baseURL!).origin);
  }
  const sprite = await page.request.get(hrefs[0]!.split("#")[0]);
  expect(sprite.status()).toBe(200);
  expect(sprite.headers()["content-type"]).toBe("image/svg+xml");
});

test("conf_a11y_01_design_catalog_accessible", async ({ page }) => {
  await page.goto("/-/design");
  await expectAccessible(page);
});
