// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
import { expect, expectAccessible, expectConfirmed, expectProblem, sideMenu, signIn, tapOrClick, test, USERS } from "../fixtures";

for (const [path, heading] of [
  ["/-/settings/profile", "Profile"],
  ["/-/settings/emails", "Email addresses"],
  ["/-/settings/security", "Security"],
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
  await expectProblem(page);
  await expect(page.getByLabel("New password", { exact: true })).toHaveAttribute("aria-invalid", "true");
  await expectAccessible(page);
});

test("conf_a11y_04_settings_navigation_by_touch_or_click", async ({ page }) => {
  await signIn(page, USERS.alice);
  await page.goto("/-/settings/profile");
  await tapOrClick(page, (await sideMenu(page, "Settings")).getByRole("link", { name: "Sessions" }));
  await expect(page.getByRole("heading", { level: 1 })).toHaveText("Sessions");
  await expect((await sideMenu(page, "Settings")).getByRole("link", { name: "Sessions" })).toHaveAttribute("aria-current", "page");
});

test("conf_a11y_04_add_and_remove_an_email_address", async ({ page }) => {
  await signIn(page, USERS.alice);
  const address = `alice-${test.info().project.name}@example.net`;
  await page.goto("/-/settings/emails");
  await page.getByRole("textbox", { name: "Email address" }).fill(address);
  await tapOrClick(page, page.getByRole("button", { name: "Add address" }));
  await expectConfirmed(page);
  const row = page.getByRole("row").filter({ hasText: address });
  await expect(row).toContainText("Not confirmed");
  await expect(row.getByRole("button", { name: /^Make primary/ })).toHaveCount(0);
  await expectAccessible(page);
  await tapOrClick(page, row.getByRole("button", { name: `Remove ${address}` }));
  await expectConfirmed(page);
  await expect(page.getByRole("row").filter({ hasText: address })).toHaveCount(0);
});

test.describe("time zone", () => {
  test.use({ timezoneId: "America/New_York" });

  test("conf_a11y_01_the_settings_propose_this_devices_zone", async ({ page }) => {
    await signIn(page, USERS.alice);
    await page.goto("/-/settings/appearance");
    const zone = page.getByLabel("Time zone");
    // Nothing saved yet: the device's zone is proposed, not saved.
    await expect(zone).toHaveValue("America/New_York");
    await expect(page.locator("[data-zone-hint]")).toContainText("America/New_York");
    await expectAccessible(page);
    await page.reload();
    await expect(zone).toHaveValue("America/New_York");
  });
});

// Picking a suggestion in a field for one name leaves just that name: no
// list separator that would make saving fail.
test("ux_single_name_fields_take_the_picked_name_alone", async ({ page }) => {
  await signIn(page, USERS.alice);
  for (const [path, label] of [
    ["/alice/demo/settings/access", "Username"],
    ["/alice/demo/merge-requests", "Author"],
  ] as const) {
    await page.goto(path);
    const field = page.getByRole("combobox", { name: label });
    if (!(await field.isVisible())) {
      await tapOrClick(page, page.getByText("Filters", { exact: true }));
    }
    await field.pressSequentially("ali");
    await expect(page.getByRole("listbox", { name: "People" }).getByRole("option").first()).toContainText("alice");
    await field.press("Enter");
    await expect(field).toHaveValue("alice");
  }
});
