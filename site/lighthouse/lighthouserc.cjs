// Lighthouse CI over the built site (site/dist, served by run.mjs): the
// home page, the docs index, a long docs page and the playground, on
// Lighthouse's mobile profile with its own simulated throttling. The
// budgets are the four category scores. Measured on the built site, every
// category of every page scored 100 over three runs a page (LCP 0.8-1.0 s,
// TBT 0, CLS 0-0.005); the floors are a little under that, so a regression
// fails and a runner's noise does not. Raise a floor when the site earns
// it, never lower one to pass.
const site = (process.env.SITE_URL || 'http://localhost:8788').replace(/\/$/, '');

module.exports = {
  ci: {
    collect: {
      url: [`${site}/`, `${site}/docs/`, `${site}/docs/fennecql`, `${site}/playground`],
      numberOfRuns: Number(process.env.LHCI_RUNS || 2),
      settings: {
        chromeFlags: '--headless=new --no-sandbox',
        onlyCategories: ['performance', 'seo', 'accessibility', 'best-practices'],
      },
    },
    assert: {
      assertions: {
        'categories:performance': ['error', { minScore: 0.9, aggregationMethod: 'median-run' }],
        'categories:accessibility': ['error', { minScore: 0.95, aggregationMethod: 'median-run' }],
        'categories:best-practices': ['error', { minScore: 0.95, aggregationMethod: 'median-run' }],
        'categories:seo': ['error', { minScore: 0.95, aggregationMethod: 'median-run' }],
        'largest-contentful-paint': ['error', { maxNumericValue: 2000, aggregationMethod: 'median' }],
        'cumulative-layout-shift': ['error', { maxNumericValue: 0.05, aggregationMethod: 'median' }],
        'total-blocking-time': ['error', { maxNumericValue: 150, aggregationMethod: 'median' }],
      },
    },
    upload: { target: 'filesystem', outputDir: '.lighthouseci/reports' },
  },
};
