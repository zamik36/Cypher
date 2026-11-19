import { defineConfig, devices } from "@playwright/test";

const CI = Boolean(process.env["CI"]);
/** Traces and reports go next to every other build artifact. */
const OUT = "../../target/playwright";

/**
 * End-to-end tests of the production bundle (`vite preview`) against a
 * running stack: the preview server forwards `/ws` and `/relay` to the
 * gateway and relay WebSocket listeners (see vite.config.ts).
 */
export default defineConfig({
  testDir: "e2e",
  timeout: 90_000,
  expect: { timeout: 20_000 },
  forbidOnly: CI,
  // Every test brings its own identities, so they share the stack safely.
  fullyParallel: true,
  ...(CI && { workers: 2 }),
  outputDir: `${OUT}/results`,
  reporter: CI ? [["github"], ["html", { open: "never", outputFolder: `${OUT}/report` }]] : "list",
  use: {
    baseURL: "http://localhost:4173",
    trace: "retain-on-failure",
  },
  projects: [
    {
      name: "chromium",
      use: {
        ...devices["Desktop Chrome"],
        // Full Chromium in its new headless mode: unlike the headless shell it
        // grants notifications, which the push test needs.
        channel: "chromium",
        launchOptions: {
          args: ["--use-fake-ui-for-media-stream", "--use-fake-device-for-media-stream"],
        },
      },
    },
  ],
  webServer: {
    command: "npm run preview",
    url: "http://localhost:4173",
    reuseExistingServer: !CI,
  },
});
