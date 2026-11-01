// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//
// htmx 4 + hx-csp behaviour on the design catalog's counter sample.
import { expect, tapOrClick, test } from "../fixtures";

test("conf_sec_04_htmx_swap_keeps_working_with_fresh_nonces", async ({ page }) => {
  await page.goto("/-/design");
  const count = page.locator("#demo-count");
  await expect(count).toHaveText("0");
  // Each swap brings a fragment stamped with its own response nonce;
  // hx-csp rewrites it, so the swapped-in link must work again.
  for (const n of ["1", "2", "3"]) {
    await tapOrClick(page, page.locator("#demo-next"));
    await expect(count).toHaveText(n);
  }
  // Swaps push the shareable URL; back and forward restore the state
  // (htmx 4 re-fetches instead of keeping snapshots in localStorage).
  expect(new URL(page.url()).search).toBe("?count=3");
  await page.goBack();
  await expect(count).toHaveText("2");
  await page.goForward();
  await expect(count).toHaveText("3");
  expect(await page.evaluate(() => Object.keys(localStorage).filter((k) => k.includes("htmx")))).toEqual([]);
});

test("conf_sec_04_hx_ignore_keeps_user_content_inert", async ({ page }) => {
  await page.goto("/-/design");
  await page.waitForFunction(() => "htmx" in window);
  // Even an element carrying the valid page nonce does nothing inside an
  // hx-ignore container (where sanitized user HTML is rendered).
  let requests = 0;
  page.on("request", (r) => {
    if (r.url().includes("count=42")) requests++;
  });
  await page.evaluate(() => {
    const nonce = (document.querySelector("script[nonce]") as HTMLScriptElement).nonce;
    const box = document.createElement("div");
    box.setAttribute("hx-ignore", "");
    const btn = document.createElement("button");
    btn.id = "inert";
    btn.type = "button";
    btn.textContent = "x";
    btn.setAttribute("hx-get", "/-/design?count=42");
    btn.setAttribute("hx-nonce", nonce);
    box.append(btn);
    document.querySelector("main")!.append(box);
    (window as unknown as { htmx: { process(e: Element): void } }).htmx.process(box);
  });
  await page.locator("#inert").click();
  await page.waitForTimeout(300);
  expect(requests).toBe(0);
});

test("conf_sec_04_htmx_sample_works_without_javascript", async ({ browser, baseURL }) => {
  const ctx = await browser.newContext({ javaScriptEnabled: false, baseURL });
  const page = await ctx.newPage();
  await page.goto("/-/design");
  await page.locator("#demo-next").click();
  await expect(page.locator("#demo-count")).toHaveText("1");
  expect(new URL(page.url()).search).toBe("?count=1");
  await ctx.close();
});

test("conf_sec_04_unstamped_element_is_stripped", async ({ page, guard }) => {
  guard.allowStrip = true;
  await page.goto("/-/design");
  await page.waitForFunction(() => "htmx" in window);
  const fired = await page.evaluate(async () => {
    const el = document.createElement("a");
    el.id = "injected";
    el.setAttribute("hx-get", "/-/design?count=99");
    el.textContent = "x";
    document.querySelector("main")!.append(el);
    let got = false;
    el.addEventListener("htmx:security:strip", () => (got = true));
    (window as unknown as { htmx: { process(e: Element): void } }).htmx.process(el);
    await new Promise((r) => setTimeout(r, 50));
    return { got, attr: el.getAttribute("hx-get") };
  });
  expect(fired).toEqual({ got: true, attr: null });
});

test("conf_sec_04_stolen_page_nonce_is_stripped", async ({ page, guard }) => {
  guard.allowStrip = true;
  await page.goto("/-/design");
  const pageNonce = await page.evaluate(() => (document.querySelector("script[nonce]") as HTMLScriptElement).nonce);
  expect(pageNonce.length).toBeGreaterThan(0);
  // A response that replays the page's own nonce (as an attacker who read it
  // would) must lose it: the server never knows the page nonce.
  await page.route("**/-/design?count=1", (route) =>
    route.fulfill({
      status: 200,
      contentType: "text/html",
      body: `<div id="demo-region"><a id="evil" hx-get="/-/design?count=5" hx-nonce="${pageNonce}">evil</a></div>`,
    }),
  );
  await tapOrClick(page, page.locator("#demo-next"));
  const evil = page.locator("#evil");
  await expect(evil).toBeAttached();
  await expect.poll(() => guard.stripped.length).toBeGreaterThan(0);
  await expect(evil).not.toHaveAttribute("hx-get");
});
