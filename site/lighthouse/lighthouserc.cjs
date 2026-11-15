// Lighthouse CI over the built site (site/dist, served by run.mjs): the
// home page, the docs index, a long docs page and the playground, on
// Lighthouse's mobile profile with its own simulated throttling. The
// budgets are the four category scores. Measured on the built site, every
// category of every page scored 100 over three runs a page (LCP 0.8-1.0 s,
// TBT 0, CLS 0-0.005). Accessibility, best practices and SEO are audits
// of the page as served, the same on every run, so they are held at 100;
// performance, LCP and TBT are timed on a shared runner, so their floors
// sit a little under what was measured, a regression failing and a
// runner's noise not. Raise a floor when the site earns it, never lower
// one to pass.
const site = (process.env.SITE_URL || 'http://localhost:8788').replace(/\/$/, '');
//
// Byte budgets, each page's transfer size as the local server (serve.mjs)
// sends it -- brotli for text, as the edge does, so what CI sees -- read from
// Lighthouse's own `total-byte-weight` and `resource-summary` on 2026-11-15
// at b6b89e02 (identical over two runs: bytes do not depend on speed), plus
// 10%, rounded up to a whole KiB. In bytes -> budget in KiB:
//
//   page             total (budget)     script (budget)   stylesheet (budget)
//   /                117761 (127)       10884 (12)        11227 (13)
//   /docs/            77858 (84)         6150 (7)         11227 (13)
//   /docs/fennecql    91796 (99)         6150 (7)         11227 (13)
//   /playground      463031 (498)       82487 (89)        19864 (22)
//   /download        206058 (222)        6150 (7)         11227 (13)
//
// When a budget fails, measure the page again (`node run.mjs` in this
// directory, the bytes from .lighthouseci/lhr-*.json) and raise it in the
// same pull request, saying why the page grew; never raise one blind.
const kib = (n) => n * 1024;
// [URL pattern, total KiB, script KiB, stylesheet KiB]; the port varies, so
// a pattern anchors on the path.
const budgets = [
  ['^https?://[^/]+/$', 127, 12, 13],
  ['/docs/$', 84, 7, 13],
  ['/docs/fennecql$', 99, 7, 13],
  ['/playground$', 498, 89, 22],
  ['/download$', 222, 7, 13],
];

module.exports = {
  ci: {
    collect: {
      url: [`${site}/`, `${site}/docs/`, `${site}/docs/fennecql`, `${site}/playground`, `${site}/download`],
      numberOfRuns: Number(process.env.LHCI_RUNS || 2),
      settings: {
        chromeFlags: '--headless=new --no-sandbox',
        onlyCategories: ['performance', 'seo', 'accessibility', 'best-practices'],
      },
    },
    // assertMatrix, not assertions beside per-page ones: Lighthouse CI
    // refuses both in one config. The first entry is every page's scores and
    // timings; the rest are each page's byte budgets.
    assert: {
      assertMatrix: [
        {
          assertions: {
            'categories:performance': ['error', { minScore: 0.9, aggregationMethod: 'median-run' }],
            'categories:accessibility': ['error', { minScore: 1, aggregationMethod: 'median-run' }],
            'categories:best-practices': ['error', { minScore: 1, aggregationMethod: 'median-run' }],
            'categories:seo': ['error', { minScore: 1, aggregationMethod: 'median-run' }],
            'largest-contentful-paint': ['error', { maxNumericValue: 2000, aggregationMethod: 'median' }],
            'cumulative-layout-shift': ['error', { maxNumericValue: 0.05, aggregationMethod: 'median' }],
            'total-blocking-time': ['error', { maxNumericValue: 150, aggregationMethod: 'median' }],
          },
        },
        ...budgets.map(([matchingUrlPattern, total, script, style]) => ({
          matchingUrlPattern,
          assertions: {
            'total-byte-weight': ['error', { maxNumericValue: kib(total), aggregationMethod: 'median' }],
            'resource-summary:script:size': ['error', { maxNumericValue: kib(script), aggregationMethod: 'median' }],
            'resource-summary:stylesheet:size': ['error', { maxNumericValue: kib(style), aggregationMethod: 'median' }],
          },
        })),
      ],
    },
    upload: { target: 'filesystem', outputDir: '.lighthouseci/reports' },
  },
};
