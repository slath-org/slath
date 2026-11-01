// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//
// Browser tests. Every test runs in Chromium, Firefox, and WebKit, each in
// four cells: {light, dark} x {desktop 1280x800 mouse, touch 390x844}.
import { defineConfig, devices, type Project } from "@playwright/test";

const browsers = [
  { name: "chromium", device: devices["Desktop Chrome"] },
  { name: "firefox", device: devices["Desktop Firefox"] },
  { name: "webkit", device: devices["Desktop Safari"] },
] as const;

const cells = [
  { name: "light-desktop", colorScheme: "light", viewport: { width: 1280, height: 800 }, hasTouch: false },
  { name: "dark-desktop", colorScheme: "dark", viewport: { width: 1280, height: 800 }, hasTouch: false },
  { name: "light-touch", colorScheme: "light", viewport: { width: 390, height: 844 }, hasTouch: true },
  { name: "dark-touch", colorScheme: "dark", viewport: { width: 390, height: 844 }, hasTouch: true },
] as const;

const projects: Project[] = browsers.flatMap((b) =>
  cells.map((c) => ({
    name: `${b.name}-${c.name}`,
    use: {
      ...b.device,
      colorScheme: c.colorScheme,
      viewport: c.viewport,
      hasTouch: c.hasTouch,
    },
  })),
);

export default defineConfig({
  testDir: "tests",
  globalSetup: "./global-setup.ts",
  fullyParallel: true,
  forbidOnly: !!process.env.CI,
  retries: process.env.CI ? 1 : 0,
  workers: process.env.CI ? 4 : undefined,
  reporter: [
    ["list"],
    ["junit", { outputFile: "../../target/e2e/junit.xml" }],
  ],
  outputDir: "../../target/e2e/results",
  use: {
    trace: "retain-on-failure",
    serviceWorkers: "block",
  },
  projects,
});
