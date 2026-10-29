#!/usr/bin/env node
// The bench comment on a pull request, from the runs of bench-actions.yml's matrix:
//   node .github/scripts/bench-comment.mjs <results-dir> <out.md>
// <results-dir> holds one directory per leg (the bench-pr-<label> artifacts), each with leg.json
// ({ label, runner, gate, threads, code }: code 0 passed, 1 a regression, 2 the bench failed,
// missing when the leg never got to measure) and comment.md (bench-gate.sh's tables).
// The gate leg's tables go between the ts-r-bench:results markers, where scripts/readme-bench.mjs
// reads them; the other legs' tables are folded under it.
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

export const MARK = '<!-- ts-r-bench -->';
// GitHub's limit is 65,536 characters; the rest of the comment is small.
const MAX_TABLES = 60_000;

const VERDICT = { 0: 'passed', 1: '**regression**', 2: '**the bench failed**' };

/** The comment body. `legs`: [{ label, runner, gate, threads, code, tables }]. */
export function buildComment(legs, { head, baseRef, base, runUrl }) {
  const sorted = [...legs].sort((a, b) => Number(b.gate) - Number(a.gate) || a.label.localeCompare(b.label));
  const gate = sorted.find((leg) => leg.gate);
  const lines = [MARK, '### Bench', ''];
  lines.push(`> \`${head.slice(0, 7)}\` against \`${baseRef}\` @ \`${base.slice(0, 7)}\`, base and PR on the same GitHub-hosted runner for each row ([run](${runUrl})).`, '');
  lines.push('| Runner | Threads | Result |', '|---|---:|---|');
  for (const leg of sorted) {
    const verdict = VERDICT[leg.code] ?? 'did not run';
    lines.push(`| ${leg.label}${leg.gate ? ' (the gate)' : ''} | ${leg.threads} | ${verdict} |`);
  }
  lines.push('');
  lines.push(gate
    ? 'Only the gate row fails the check. The others report: shared runners on those systems are too noisy to gate on (bench/README.md, "Benches on GitHub-hosted runners").'
    : 'No gate row ran.');
  lines.push('');
  let budget = MAX_TABLES;
  const take = (text) => {
    const body = text.trim();
    if (body.length <= budget) {
      budget -= body.length;
      return body;
    }
    const cut = body.slice(0, Math.max(0, budget));
    budget = 0;
    return `${cut}\n\n_(truncated; the full tables are in the run's artifacts)_`;
  };
  if (gate?.tables?.trim()) {
    lines.push(`<!-- ts-r-bench:machine a GitHub-hosted ${gate.runner} runner -->`);
    lines.push('<!-- ts-r-bench:results -->', take(gate.tables), '<!-- /ts-r-bench:results -->', '');
  }
  for (const leg of sorted) {
    if (leg === gate || !leg.tables?.trim()) continue;
    lines.push(`<details><summary>${leg.label}, ${leg.threads} thread${leg.threads === 1 ? '' : 's'}: ${VERDICT[leg.code] ?? 'did not run'}</summary>`, '');
    lines.push(take(leg.tables), '', '</details>', '');
  }
  return `${lines.join('\n').trimEnd()}\n`;
}

/** The legs in a results directory; a leg whose files are missing or malformed is skipped. */
export function readLegs(dir) {
  const legs = [];
  for (const name of fs.existsSync(dir) ? fs.readdirSync(dir).sort() : []) {
    let leg;
    try {
      leg = JSON.parse(fs.readFileSync(path.join(dir, name, 'leg.json'), 'utf8'));
    } catch {
      continue;
    }
    if (typeof leg?.label !== 'string') continue;
    let tables = '';
    try {
      tables = fs.readFileSync(path.join(dir, name, 'comment.md'), 'utf8');
    } catch {}
    legs.push({ label: leg.label, runner: String(leg.runner ?? leg.label), gate: leg.gate === true, threads: Number(leg.threads) || 0, code: leg.code, tables });
  }
  return legs;
}

if (process.argv[1] && fileURLToPath(import.meta.url) === path.resolve(process.argv[1])) {
  const [dir, out] = process.argv.slice(2);
  const env = process.env;
  const body = buildComment(readLegs(dir), { head: env.HEAD_SHA ?? '', baseRef: env.BASE_REF ?? '', base: env.BASE_SHA ?? '', runUrl: env.RUN_URL ?? '' });
  fs.writeFileSync(out, body);
}
