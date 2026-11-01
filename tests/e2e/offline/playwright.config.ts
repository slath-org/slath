// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//
// The offline render check (scripts/test-offline.sh): the release image
// with outbound networking disabled, one browser, sequential tests.
import { defineConfig, devices } from "@playwright/test";

export default defineConfig({
  testDir: ".",
  fullyParallel: false,
  workers: 1,
  reporter: [["list"]],
  outputDir: "../../../target/e2e/offline-results",
  use: { ...devices["Desktop Chrome"], baseURL: process.env.SLATH_OFFLINE_URL, serviceWorkers: "block" },
});
