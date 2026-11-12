// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//
// Webhook settings: adding one shows its generated secret once, a ping
// shows in the delivery log, a delivery has its own page, and errors are
// announced. Each project uses its own URL and deletes its webhook.
import { USERS, expect, expectAccessible, signIn, tapOrClick, test } from "../fixtures";

test("conf_a11y_01_webhook_settings", async ({ page }, info) => {
  await signIn(page, USERS.alice);
  await page.goto("/alice/demo/settings/webhooks");
  await expect(page.getByRole("heading", { level: 1 })).toHaveText("Webhooks");
  await expectAccessible(page);

  // A missing URL is announced.
  await tapOrClick(page, page.getByRole("button", { name: "Add webhook" }));
  await expect(page.getByRole("alert")).toBeVisible();
  await expect(page.getByLabel("Payload URL")).toHaveAttribute("aria-invalid", "true");
  await expectAccessible(page);

  const url = `http://127.0.0.1:9/${info.project.name}`;
  await page.getByLabel("Payload URL").fill(url);
  await page.getByRole("checkbox", { name: /^issue\b/ }).check();
  await tapOrClick(page, page.getByRole("button", { name: "Add webhook" }));
  await expect(page.getByText("This is its secret.")).toBeVisible();
  await expect(page.getByLabel("Secret", { exact: true }).first()).toHaveValue(/.{16,}/);
  await expectAccessible(page);

  // A ping shows in the log (nothing listens, so it fails) and has a page.
  await tapOrClick(page, page.getByRole("button", { name: "Send a ping" }));
  await expect(page.getByText("Ping sent.")).toBeVisible();
  const row = page.getByRole("row").filter({ hasText: "ping" });
  await expect(row).toBeVisible();
  await tapOrClick(page, row.getByRole("link"));
  await expect(page.getByRole("heading", { level: 1 })).toContainText("Delivery");
  await expect(page.getByRole("heading", { name: "Payload" })).toBeVisible();
  await expectAccessible(page);

  // Deleting it leaves the list as it was.
  await tapOrClick(page, page.getByRole("link", { name: /^Webhook to/ }));
  await tapOrClick(page, page.getByRole("button", { name: "Delete webhook" }));
  await expect(page.getByText("Webhook deleted.")).toBeVisible();
  await expect(page.getByRole("link", { name: url })).toHaveCount(0);
});
