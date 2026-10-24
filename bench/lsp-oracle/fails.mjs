// Lists failing lsp-oracle cases from a results file, one line each.
// Usage: node bench/lsp-oracle/fails.mjs [REGEX] [results.json]
//   REGEX matches `<feature>:<test name>` (for example `^hover:` or `rename:.*Jsx`).

import fs from 'node:fs';

const re = process.argv[2] ? new RegExp(process.argv[2]) : null;
const file = process.argv[3] ?? 'target/lsp-oracle/results.json';
const { results } = JSON.parse(fs.readFileSync(file, 'utf8'));
for (const f of results.filter((r) => r.status === 'fail')) {
  const k = `${f.feature}:${f.name}`;
  if (re && !re.test(k)) continue;
  console.log('##', k, `(${f.agree ?? 0}/${f.queries ?? 0})`, '|', (f.detail ?? '').replace(/\n/g, ' || ').slice(0, 300));
}
