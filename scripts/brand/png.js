// Renders docs/brand/dtop-icon.svg to PNGs with Playwright's Chromium (transparent corners).
// Usage: node scripts/brand/png.js   (build.py --png runs it for you)
const fs = require('fs');
const path = require('path');
const { chromium } = require('playwright');

const out = path.resolve(__dirname, '..', '..', 'docs', 'brand');
const icon = fs.readFileSync(path.join(out, 'dtop-icon.svg'), 'utf8');

(async () => {
  const browser = await chromium.launch();
  const page = await browser.newPage();
  for (const size of [1024, 512, 256, 128, 64, 32]) {
    await page.setViewportSize({ width: size, height: size });
    await page.setContent('<style>html,body{margin:0;background:transparent}' +
      'svg{display:block;width:100vw;height:100vh}</style>' + icon);
    const target = path.join(out, `dtop-icon-${size}.png`);
    await page.screenshot({ path: target, omitBackground: true });
    console.log('wrote', path.relative(path.resolve(out, '..', '..'), target));
  }
  await browser.close();
})();
