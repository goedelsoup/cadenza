import { defineConfig, devices } from '@playwright/test'

// Cadenza web e2e config. The dev server is launched by Playwright on a
// fixed port; tests mock /api/compose so no ANTHROPIC_API_KEY is required.
const PORT = 5179
const baseURL = `http://localhost:${PORT}`

export default defineConfig({
  testDir: './tests/e2e',
  fullyParallel: true,
  forbidOnly: !!process.env.CI,
  retries: process.env.CI ? 2 : 0,
  workers: process.env.CI ? 1 : undefined,
  reporter: process.env.CI ? [['github'], ['html', { open: 'never' }]] : 'list',
  use: {
    baseURL,
    trace: 'retain-on-failure',
    video: 'retain-on-failure',
  },
  projects: [
    { name: 'chromium', use: { ...devices['Desktop Chrome'] } },
  ],
  webServer: {
    // Use vite directly so we don't depend on the cadenza-wasm:wasm:build:dev
    // task graph for headless e2e — the WASM bridge degrades gracefully when
    // the module is missing, which is exactly the surface we want to test.
    command: `pnpm exec vite dev --port ${PORT} --strictPort`,
    url: baseURL,
    reuseExistingServer: !process.env.CI,
    timeout: 120_000,
    env: {
      // The /api/compose route constructs an Anthropic client at module load
      // time; supply a placeholder so import doesn't blow up. All requests
      // are intercepted by page.route() before reaching the handler.
      ANTHROPIC_API_KEY: 'sk-test-placeholder',
    },
  },
})
