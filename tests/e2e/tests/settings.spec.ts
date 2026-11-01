// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
import { USERS, expect, expectAccessible, signIn, tapOrClick, test } from "../fixtures";

for (const [path, heading] of [
  ["/-/settings/profile", "Profile"],
  ["/-/settings/security", "Password"],
  ["/-/settings/sessions", "Sessions"],
  ["/-/settings/keys", "SSH keys"],
  ["/-/settings/tokens", "Access tokens"],
  ["/-/settings/notifications", "Notifications"],
  ["/-/notifications", "Notifications"],
  ["/-/notifications?all=true", "Notifications"],
  ["/-/search?q=demo", "Search"],
  ["/-/import", "Import a repository"],
  ["/-/search?q=demo&type=repos", "Search"],
  ["/-/sudo?next=/", "Confirm your password"],
] as const) {
  test(`conf_a11y_01_settings_page_accessible ${path}`, async ({ page }) => {
    await signIn(page, USERS.alice);
    await page.goto(path);
    await expect(page.getByRole("heading", { level: 1 })).toHaveText(heading);
    await expectAccessible(page);
  });
}

test("conf_a11y_01_settings_validation_errors_are_announced", async ({ page }) => {
  await signIn(page, USERS.alice);
  await page.goto("/-/settings/security");
  await page.getByLabel("New password", { exact: true }).fill("short");
  await page.getByLabel("Confirm new password").fill("different");
  await tapOrClick(page, page.getByRole("button", { name: "Change password" }));
  await expect(page.getByRole("alert")).toContainText("Use at least 12 characters.");
  await expect(page.getByLabel("New password", { exact: true })).toHaveAttribute("aria-invalid", "true");
  await expectAccessible(page);
});

test("conf_a11y_04_settings_navigation_by_touch_or_click", async ({ page }) => {
  await signIn(page, USERS.alice);
  await page.goto("/-/settings/profile");
  const nav = page.getByRole("navigation", { name: "Settings" });
  await tapOrClick(page, nav.getByRole("link", { name: "Sessions" }));
  await expect(page.getByRole("heading", { level: 1 })).toHaveText("Sessions");
  await expect(nav.getByRole("link", { name: "Sessions" })).toHaveAttribute("aria-current", "page");
});
