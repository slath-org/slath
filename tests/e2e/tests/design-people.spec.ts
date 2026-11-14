// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//
// The catalog's people family: an avatar before every account name,
// drawn in the theme's tones, one link per account; Git identities get a
// neutral icon.
import { expect, expectAccessible, test } from "../fixtures";

test("conf_design_03_people_family_accessible", async ({ page }) => {
  await page.goto("/-/design/people");
  await expect(page.getByRole("heading", { level: 1 })).toHaveText("People");
  await expectAccessible(page);
});

test("conf_design_03_avatar_before_every_account_name", async ({ page }) => {
  await page.goto("/-/design/people");
  const accounts = page.locator("section[aria-labelledby=account] figure .account");
  expect(await accounts.count()).toBeGreaterThanOrEqual(5);
  for (const a of await accounts.all()) {
    // The avatar comes first and is hidden from screen readers.
    const first = a.locator(":scope > :first-child");
    await expect(first).toHaveClass(/avatar/);
  }
  // One link per account, named by the account's name.
  await expect(page.getByRole("link", { name: "bob", exact: true })).toHaveCount(1);
  // The server knows bots from a list it refreshes every 10 seconds; the
  // test server creates its bots from the command line, which the list
  // only picks up then.
  const badge = page.locator("section[aria-labelledby=account] figure .badge");
  await expect(async () => {
    await page.reload();
    await expect(badge).toHaveText("Bot", { timeout: 1000 });
  }).toPass({ timeout: 20_000 });
});

test("conf_design_03_generated_avatar_follows_the_theme", async ({ page }) => {
  await page.goto("/-/design/people");
  const { avatar, body } = await page.locator(".account svg.avatar-generated").first().evaluate((el) => {
    // Lightness 0-1 of a computed color in rgb() or color(srgb ...) form.
    const light = (c: string) => {
      const n = (c.match(/[\d.]+/g) ?? []).map(Number);
      const v = c.startsWith("color(") ? n.slice(0, 3) : n.slice(0, 3).map((x) => x / 255);
      return (v[0] + v[1] + v[2]) / 3;
    };
    return { avatar: light(getComputedStyle(el).backgroundColor), body: light(getComputedStyle(document.body).backgroundColor) };
  });
  // The pattern's ground is light on a light page and dark on a dark one.
  expect(avatar > 0.5).toBe(body > 0.5);
});

test("conf_design_03_git_identity_and_avatar_group", async ({ page }) => {
  await page.goto("/-/design/people");
  const id = page.locator(".git-identity");
  await expect(id).toContainText("Alice Example");
  await expect(id.locator("img, svg.avatar")).toHaveCount(0);
  const many = page.locator(".avatar-group").nth(1);
  await expect(many.locator(".avatar")).toHaveCount(3);
  await expect(many.locator(".avatar-group-more")).toHaveText("+2");
  await expect(many.locator(".visually-hidden")).toHaveText("alice, bob, carol, dee, eve");
});
