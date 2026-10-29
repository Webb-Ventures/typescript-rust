#!/usr/bin/env node
// Keeps the benchmark tables in README.md current. Two sections, each between markers:
//
//   <!-- bench:main:start --> ... <!-- bench:main:end -->        unreleased main
//   <!-- bench:release:start --> ... <!-- bench:release:end -->  the latest release
//
// node scripts/readme-bench.mjs main <README.md> <bench-comment.md> [--date YYYY-MM-DD]
//   Rewrites the main section from a PR's bench comment (bench-actions.yml; the markdown after
//   <!-- ts-r-bench:results -->): per scenario, ts-r's median next to the fastest other tool,
//   and the machine the comment names (<!-- ts-r-bench:machine ... -->; the review box's older
//   comments name none: four dedicated cores).
//   The public mirror's sync runs it with the bench of the pull request it carries over.
// node scripts/readme-bench.mjs release <README.md> <released README.md> <version>
//   Copies the main section of the README as released into the release section. The release
//   workflow runs it, so the release table is what main measured when the release was cut.
import fs from 'node:fs';
import { fileURLToPath } from 'node:url';

const marks = (name) => [`<!-- bench:${name}:start -->`, `<!-- bench:${name}:end -->`];

/** The text between a section's markers, or null when the README has no such section. */
export function section(readme, name) {
  const [start, end] = marks(name);
  const a = readme.indexOf(start);
  const b = readme.indexOf(end);
  if (a < 0 || b < a) return null;
  return readme.slice(a + start.length, b).trim();
}

/** `readme` with a section's contents replaced; throws when the markers are missing. */
export function replaceSection(readme, name, body) {
  const [start, end] = marks(name);
  const a = readme.indexOf(start);
  const b = readme.indexOf(end);
  if (a < 0 || b < a) throw new Error(`README has no ${start} ... ${end} section`);
  return `${readme.slice(0, a + start.length)}\n${body.trim()}\n${readme.slice(b)}`;
}

/** Milliseconds from a median cell ("791.3 ms", "5.5 s"), or null. */
function ms(cell) {
  const m = /^([\d.,]+)\s*(ms|s)$/.exec(cell.trim());
  if (!m) return null;
  const n = Number(m[1].replace(/,/g, ''));
  return m[2] === 's' ? n * 1000 : n;
}

function duration(v) {
  if (v >= 1000) return `${(v / 1000).toFixed(2)} s`;
  if (v >= 10) return `${Math.round(v)} ms`;
  return `${v.toFixed(2)} ms`;
}

/** Rows of every results table in a bench comment: { scenario, tool, ours, median }. */
export function parseBench(comment) {
  const at = comment.indexOf('<!-- ts-r-bench:results -->');
  const text = at < 0 ? comment : comment.slice(at);
  const platform = /### Benchmarks \([^,)]*,\s*([^)]+)\)/.exec(text)?.[1]?.trim() ?? null;
  const machine = /<!-- ts-r-bench:machine ([^>]*?) -->/.exec(comment)?.[1]?.trim() || 'four dedicated cores';
  const rows = [];
  for (const line of text.split('\n')) {
    if (!line.startsWith('|')) continue;
    const cells = line.split('|').slice(1, -1).map((c) => c.trim());
    if (cells.length < 3 || cells[0] === 'Scenario' || /^-+:?$/.test(cells[0].replace(/^:/, ''))) continue;
    const median = ms(cells[2]);
    const ours = /^\*\*.+\*\*$/.test(cells[1]);
    rows.push({ scenario: cells[0], tool: cells[1].replace(/\*\*/g, ''), ours, median });
  }
  return { platform, machine, rows };
}

// Left out of the README: the lint scenarios compare different rule sets (lint-trpc) or a
// warm cache against a cold run (lint-trpc-same-rules), so their headline ratio would overstate
// ts-r. bench/README.md and the bench comments show them in full.
const NOT_LIKE_FOR_LIKE = /^lint-/;

/** The README table: per scenario, ts-r's first (cold) tool against the fastest other tool. */
export function benchTable(comment, date) {
  const { platform, machine, rows } = parseBench(comment);
  const scenarios = [...new Set(rows.map((r) => r.scenario))].filter((s) => !NOT_LIKE_FOR_LIKE.test(s));
  const lines = [];
  for (const s of scenarios) {
    const ours = rows.find((r) => r.scenario === s && r.ours);
    const others = rows.filter((r) => r.scenario === s && !r.ours && r.median != null).sort((a, b) => a.median - b.median);
    if (!ours || ours.median == null || others.length === 0) continue;
    const other = others[0];
    const ratio = other.median / ours.median;
    const verdict = ratio >= 1 ? `**${ratio.toFixed(1)}× faster**` : `${(1 / ratio).toFixed(1)}× slower`;
    lines.push(`| \`${s}\` | \`${ours.tool}\` ${duration(ours.median)} | \`${other.tool}\` ${duration(other.median)} | ${verdict} |`);
  }
  if (lines.length === 0) return null;
  const where = platform ? ` on ${platform}` : '';
  const when = date ? `, measured ${date}` : '';
  return [
    '| Benchmark | ts-r | Fastest other tool | ts-r is |',
    '|---|---:|---:|---:|',
    ...lines,
    '',
    `Medians${where} (${machine})${when}. Scenarios: [bench/scenarios.toml](bench/scenarios.toml) and [bench/repos.toml](bench/repos.toml).`,
  ].join('\n');
}

function main(argv) {
  const [mode, readmePath, ...rest] = argv;
  const readme = fs.readFileSync(readmePath, 'utf8');
  let body;
  if (mode === 'main') {
    const [commentPath, flag, date] = rest;
    body = benchTable(fs.readFileSync(commentPath, 'utf8'), flag === '--date' ? date : null);
    if (!body) {
      console.error('readme-bench: no comparable rows in the bench comment; README unchanged');
      return;
    }
  } else if (mode === 'release') {
    const [releasedPath, version] = rest;
    const released = section(fs.readFileSync(releasedPath, 'utf8'), 'main');
    if (!released) throw new Error(`${releasedPath} has no main bench section`);
    body = `Release **${version}**:\n\n${released}`;
  } else {
    throw new Error('usage: readme-bench.mjs main <README> <comment.md> [--date D] | release <README> <released README> <version>');
  }
  fs.writeFileSync(readmePath, replaceSection(readme, mode, body));
}

if (process.argv[1] && fileURLToPath(import.meta.url) === fs.realpathSync(process.argv[1])) {
  try {
    main(process.argv.slice(2));
  } catch (error) {
    console.error(`readme-bench: ${error.message}`);
    process.exit(1);
  }
}
