// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//
// Passkeys with Chromium's virtual authenticator (CDP): adding one, signing
// in with it alone (passwordless), and as the second step after the
// password. Firefox and WebKit have no virtual authenticator in Playwright.
import { USERS, expect, expectAccessible, signIn, tapOrClick, test } from "../fixtures";
import type { Page } from "@playwright/test";

async function virtualAuthenticator(page: Page): Promise<void> {
  const cdp = await page.context().newCDPSession(page);
  await cdp.send("WebAuthn.enable");
  await cdp.send("WebAuthn.addVirtualAuthenticator", {
    options: {
      protocol: "ctap2",
      transport: "internal",
      hasResidentKey: true,
      hasUserVerification: true,
      isUserVerified: true,
      automaticPresenceSimulation: true,
    },
  });
}

/** Creates a user as the admin and returns its name and password. */
async function newUser(page: Page, testInfo: { project: { name: string } }): Promise<{ name: string; password: string }> {
  const name = `pk-${testInfo.project.name.replace(/[^a-z0-9]/g, "")}-${Date.now().toString(36)}`.slice(0, 39);
  await signIn(page, USERS.admin);
  await page.goto("/-/admin/users");
  await page.getByLabel("Username").fill(name);
  await page.getByRole("button", { name: "Create user" }).click();
  const password = await page.locator("#new-password").inputValue();
  await page.context().clearCookies();
  return { name, password };
}

async function signOut(page: Page): Promise<void> {
  await page.getByRole("button", { name: "Sign out" }).click();
}

test("conf_auth_02_passkey_passwordless_and_second_factor", async ({ page, browserName }, testInfo) => {
  test.skip(browserName !== "chromium", "virtual authenticators are Chromium-only");
  await virtualAuthenticator(page);
  const user = await newUser(page, testInfo);
  await signIn(page, user);

  await page.goto("/-/settings/security");
  await expectAccessible(page);
  await page.getByLabel("Name for the new passkey").fill("Virtual");
  await tapOrClick(page, page.getByRole("button", { name: "Add a passkey" }));
  await expect(page.getByRole("status").filter({ hasText: "Passkey added." })).toBeVisible();
  await expect(page.getByRole("cell", { name: "Virtual", exact: true })).toBeVisible();
  await expectAccessible(page);

  // Passwordless.
  await signOut(page);
  await page.goto("/-/login");
  await tapOrClick(page, page.getByRole("button", { name: "Sign in with a passkey" }));
  await expect(page.getByRole("navigation", { name: "Main" }).getByRole("link", { name: user.name, exact: true })).toBeVisible();

  // Password first, then the passkey as the second step.
  await signOut(page);
  await page.goto("/-/login");
  await page.getByLabel("Username").fill(user.name);
  await page.getByLabel("Password").fill(user.password);
  await page.getByRole("button", { name: "Sign in", exact: true }).click();
  await expect(page.getByRole("heading", { level: 1 })).toHaveText("Two-factor authentication");
  await expectAccessible(page);
  await tapOrClick(page, page.getByRole("button", { name: "Use a passkey" }));
  await expect(page.getByRole("navigation", { name: "Main" }).getByRole("link", { name: user.name, exact: true })).toBeVisible();
});
