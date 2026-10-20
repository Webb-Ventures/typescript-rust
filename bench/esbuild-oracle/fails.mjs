// Lists failing esbuild-oracle cases from a results file, one line each.
// Usage: node bench/esbuild-oracle/fails.mjs [REGEX] [results.json]

import fs from 'node:fs';

const re = process.argv[2] ? new RegExp(process.argv[2]) : null;
const file = process.argv[3] ?? 'target/esbuild-oracle/results.json';
const { results } = JSON.parse(fs.readFileSync(file, 'utf8'));
for (const f of results.filter((r) => r.status === 'fail')) {
  const k = `${f.file.replace('bundler_', '').replace('_test.go', '')}:${f.name}`;
  if (re && !re.test(k)) continue;
  let d = f.detail;
  if (/ts-r-build failed/.test(d)) d = `TSR-ERR ${d.split('\n').find((l) => /ERROR/.test(l)) ?? ''}`;
  else if (/esbuild failed/.test(d)) d = `ESB-ERR ${d.split('\n').find((l) => /ERROR/.test(l)) ?? ''}`;
  console.log('##', k, '|', d.replace(/\n/g, ' || ').slice(0, 260));
}
