import { defineConfig, devices } from "@playwright/test";

// End-to-end tests run the built app against a real API on the synthetic
// fixtures. With PW_BASE_URL (CI), the API itself serves the build, as in
// production: `USNM_SITE_DIR=web/dist cargo run -p usnm-api`. Without it,
// `vite preview` serves the build and proxies `/v1` to a running API.
const baseURL = process.env.PW_BASE_URL ?? "http://127.0.0.1:4173";

export default defineConfig({
  testDir: "e2e",
  timeout: 30_000,
  retries: 0,
  // CI: one worker per core (4 on GitHub's runners). Locally, 4 workers ran
  // the suite in 50 s against 70 s for Playwright's default of half the cores.
  workers: process.env.CI ? "100%" : undefined,
  reporter: process.env.CI ? [["line"], ["html", { open: "never" }]] : "list",
  use: {
    baseURL,
    trace: "retain-on-failure",
    launchOptions: process.env.PW_CHROMIUM_PATH
      ? { executablePath: process.env.PW_CHROMIUM_PATH }
      : undefined,
  },
  projects: [
    { name: "desktop", use: { ...devices["Desktop Chrome"] } },
    { name: "mobile", use: { ...devices["Pixel 7"] } },
  ],
  webServer: process.env.PW_BASE_URL
    ? undefined
    : {
        command: "npm run preview -- --host 127.0.0.1 --port 4173 --strictPort",
        url: "http://127.0.0.1:4173",
        reuseExistingServer: !process.env.CI,
      },
});
