#!/usr/bin/env node
// scripts/screenshot.js
//
// Bakes the admin screenshot used in the landing page hero.
//
// Usage (from umbra_website/):
//   1. Log in once via the dev server to create a session cookie.
//   2. Run: npx playwright install chromium   (first time only)
//   3. Run: node scripts/screenshot.js
//
// The script logs in as the superuser (env UMBRA_ADMIN_USER /
// UMBRA_ADMIN_PASSWORD), captures /admin/, writes to
// static/img/admin.png at 1200x780, then prints the path.
//
// If the script cannot reach the server or login fails, it exits
// non-zero with a clear message — it does NOT write a stale PNG.

const { chromium } = require('playwright');
const path = require('path');
const fs = require('fs');

const BASE = process.env.UMBRA_BASE_URL || 'http://127.0.0.1:8000';
const USER = process.env.UMBRA_ADMIN_USER || 'admin';
const PASS = process.env.UMBRA_ADMIN_PASSWORD || 'admin';
const OUT = path.resolve(__dirname, '..', 'static', 'img', 'admin.png');

(async () => {
  fs.mkdirSync(path.dirname(OUT), { recursive: true });

  const browser = await chromium.launch();
  const context = await browser.newContext({
    viewport: { width: 1200, height: 780 },
    deviceScaleFactor: 2,
  });
  const page = await context.newPage();

  try {
    // Reach the server first — fail fast with a useful message.
    await page.goto(`${BASE}/admin/login/`, { timeout: 5000, waitUntil: 'domcontentloaded' });
  } catch (e) {
    await browser.close();
    console.error(`\n[screen] Could not reach ${BASE}/admin/login/.`);
    console.error('[screen] Is the dev server running? Try:');
    console.error('        cargo run -- serve\n');
    process.exit(1);
  }

  try {
    await page.fill('input[name=username]', USER);
    await page.fill('input[name=password]', PASS);
    await page.click('input[type=submit]');
    await page.waitForURL(/\/admin\/?$/, { timeout: 5000 });
  } catch (e) {
    await browser.close();
    console.error(`\n[screen] Login failed for user "${USER}".`);
    console.error('[screen] Set UMBRA_ADMIN_USER and UMBRA_ADMIN_PASSWORD, or');
    console.error('[screen] make sure the superuser exists (cargo run -- createsuperuser).\n');
    process.exit(2);
  }

  await page.waitForLoadState('networkidle');
  await page.screenshot({ path: OUT, fullPage: false });
  await browser.close();

  const stat = fs.statSync(OUT);
  console.log(`[screen] wrote ${OUT} (${(stat.size / 1024).toFixed(1)} KB)`);
})();
