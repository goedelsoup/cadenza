import { test, expect, type Page } from '@playwright/test'
import type { AiPhrase } from '@cadenza/types'

// Wait until Svelte's onMount has run on the index route. Without this,
// clicks can race the hydration of event listeners and silently no-op.
// onMount logs "cadenza ready" into the footer log column, which is the
// most reliable signal that the client is fully wired up.
async function waitForHydration(page: Page) {
  await page.goto('/')
  await expect(page.locator('footer').getByText('cadenza ready')).toBeVisible()
}

// A canned AiPhrase the mocked /api/compose endpoint streams back as plain
// text. The frontend's streamProxy concatenates the body and JSON.parses it.
const FAKE_PHRASE: AiPhrase = {
  type: 'chord progression',
  summary: 'ii–V–I in C major',
  key: 'C major',
  tempo: 100,
  time_signature: '4/4',
  bars: 4,
  chords: ['Dm7', 'G7', 'Cmaj7'],
  notes: [
    { pitch: 62, start: 0, dur: 1, vel: 80 },
    { pitch: 67, start: 1, dur: 1, vel: 80 },
    { pitch: 60, start: 2, dur: 2, vel: 80 },
  ],
}

test.describe('cadenza-web smoke', () => {
  test.beforeEach(async ({ page }) => {
    // Stub the SvelteKit compose endpoint. The real handler streams text
    // chunks; we return the entire JSON in one body since the client just
    // concatenates and parses.
    await page.route('**/api/compose', async (route) => {
      await route.fulfill({
        status: 200,
        contentType: 'text/plain; charset=utf-8',
        body: JSON.stringify(FAKE_PHRASE),
      })
    })
  })

  test('renders header, controls, and empty state', async ({ page }) => {
    await waitForHydration(page)

    await expect(page.locator('header .logo')).toContainText('caden')
    await expect(page.locator('header .badge')).toHaveText('v0.1')
    await expect(page.locator('.daemon-pill')).toBeVisible()
    await expect(page.locator('.ctx-pill')).toContainText('context:')

    // Sidebar controls
    await expect(page.locator('aside select').first()).toBeVisible()
    await expect(page.locator('aside .tags .tag')).toHaveCount(10)

    // Empty phrases state
    await expect(page.locator('.empty')).toContainText('set parameters')
  })

  test('toggling a style tag flips its active state', async ({ page }) => {
    await waitForHydration(page)
    // defaultSession() seeds styles with ['jazz','modal'], so jazz starts
    // active. Verify a click flips it off, and another flips it back on.
    const jazz = page.locator('aside .tags .tag', { hasText: 'jazz' })
    await expect(jazz).toHaveClass(/active/)
    await jazz.click()
    await expect(jazz).not.toHaveClass(/active/)
    await jazz.click()
    await expect(jazz).toHaveClass(/active/)

    // A style not in the default seed: blues starts inactive.
    const blues = page.locator('aside .tags .tag', { hasText: 'blues' })
    await expect(blues).not.toHaveClass(/active/)
    await blues.click()
    await expect(blues).toHaveClass(/active/)
  })

  test('compose flow renders a phrase card from a mocked response', async ({ page }) => {
    await waitForHydration(page)
    await expect(page.locator('.empty')).toBeVisible()
    await expect(page.locator('button.btn-compose')).toBeEnabled()

    const textarea = page.getByPlaceholder(/describe what you want/)
    await textarea.click()
    await textarea.fill('something jazzy in C')
    await expect(textarea).toHaveValue('something jazzy in C')

    const composeRequest = page.waitForRequest('**/api/compose')
    await page.locator('button.btn-compose').click()
    await composeRequest

    // The new card should render and the empty state should disappear.
    const card = page.locator('.phrase-card').first()
    await expect(card).toBeVisible()
    await expect(page.locator('.empty')).toHaveCount(0)
    await expect(card.locator('.phrase-label')).toHaveText('chord progression')
    await expect(card.locator('.phrase-meta')).toContainText('bars')
    await expect(card.locator('.chord')).toHaveCount(3)
    await expect(card.locator('.chord').first()).toHaveText('Dm7')
  })
})
