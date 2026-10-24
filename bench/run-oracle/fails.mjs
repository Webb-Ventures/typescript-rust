// Lists failing ts-r-run oracle cases from a results file, one line each.
// Usage: node bench/run-oracle/fails.mjs [REGEX] [results.json]
//   --skips lists skipped cases with their reasons instead.

import fs from 'node:fs';

const argv = process.argv.slice(2);
const skips = argv.includes('--skips');
const [pattern, file = 'target/run-oracle/results.json'] = argv.filter((a) => a !== '--skips');
const re = pattern ? new RegExp(pattern) : null;
const { cases } = JSON.parse(fs.readFileSync(file, 'utf8'));
for (const c of cases.filter((x) => x.status === (skips ? 'skip' : 'fail'))) {
  const k = `${c.source}/${c.file}: ${c.name}`;
  if (re && !re.test(k)) continue;
  const d = skips ? c.reason : (c.detail ?? '');
  console.log('##', k, '|', String(d).replace(/\n/g, ' || ').slice(0, 300));
}
