import { defineConfig, devices } from "@playwright/test";

export default defineConfig({
  testDir: "./tests/e2e",
  fullyParallel: true,
  forbidOnly: Boolean(process.env.CI),
  retries: process.env.CI ? 2 : 0,
  reporter: [["html", { open: "never" }]],
  use: {
    baseURL: "http://127.0.0.1:4320",
    colorScheme: "light",
    contextOptions: { reducedMotion: "reduce" },
    timezoneId: "UTC",
    trace: "on-first-retry",
  },
  projects: [
    {
      name: "desktop-en-US",
      use: {
        ...devices["Desktop Chrome"],
        deviceScaleFactor: 1,
        locale: "en-US",
        viewport: { width: 1440, height: 1000 },
      },
    },
    {
      name: "desktop-zh-CN",
      use: {
        ...devices["Desktop Chrome"],
        deviceScaleFactor: 1,
        locale: "zh-CN",
        viewport: { width: 1440, height: 1000 },
      },
    },
    {
      name: "mobile-en-US",
      use: {
        ...devices["Pixel 7"],
        deviceScaleFactor: 1,
        locale: "en-US",
        viewport: { width: 390, height: 844 },
      },
    },
    {
      name: "mobile-zh-CN",
      use: {
        ...devices["Pixel 7"],
        deviceScaleFactor: 1,
        locale: "zh-CN",
        viewport: { width: 390, height: 844 },
      },
    },
  ],
  webServer: {
    command: "pnpm preview",
    url: "http://127.0.0.1:4320",
    reuseExistingServer: false,
  },
});
