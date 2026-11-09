// Each page's median run from .lighthouseci/: the four scores and the
// numbers behind the performance one, as a Markdown table.
import { readdirSync, readFileSync } from 'node:fs';

const dir = '.lighthouseci';
const byUrl = new Map();
for (const f of readdirSync(dir).filter((f) => f.startsWith('lhr-') && f.endsWith('.json'))) {
  const r = JSON.parse(readFileSync(`${dir}/${f}`, 'utf8'));
  const u = r.finalDisplayedUrl ?? r.finalUrl;
  byUrl.set(u, [...(byUrl.get(u) ?? []), r]);
}
const median = (xs) => [...xs].sort((a, b) => a - b)[Math.floor(xs.length / 2)];
console.log('| page | perf | a11y | best practices | SEO | FCP | LCP | TBT | CLS |');
console.log('| --- | --- | --- | --- | --- | --- | --- | --- | --- |');
for (const [url, rs] of byUrl) {
  const lcp = median(rs.map((r) => r.audits['largest-contentful-paint'].numericValue));
  const r = rs.find((x) => x.audits['largest-contentful-paint'].numericValue === lcp);
  const score = (c) => rs.map((x) => Math.round((x.categories[c]?.score ?? 0) * 100)).join('/');
  console.log(
    `| \`${new URL(url).pathname}\` | ${score('performance')} | ${score('accessibility')} | ${score('best-practices')} | ${score('seo')} | ` +
      `${(r.audits['first-contentful-paint'].numericValue / 1000).toFixed(2)} s | ${(lcp / 1000).toFixed(2)} s | ` +
      `${Math.round(r.audits['total-blocking-time'].numericValue)} ms | ${r.audits['cumulative-layout-shift'].numericValue.toFixed(3)} |`,
  );
}
