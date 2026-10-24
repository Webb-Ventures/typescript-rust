// Runs one of tsx's spec files against one subject and writes the per-test results as JSON.
//
//   node --import ./register.mjs spec-runner.mjs --spec smoke --subject tsx|ts-r-run \
//     --out FILE [--filter REGEX]
//
// Started by run.mjs with cwd = target/upstream/tsx (the suite resolves tests/fixtures/ from it).

import fs from 'node:fs';
import path from 'node:path';
import { pathToFileURL } from 'node:url';
import { runner, settle } from './manten.mjs';
import { createNode } from './tsx.mjs';

const args = process.argv.slice(2);
const opt = (name) => {
  const i = args.indexOf(`--${name}`);
  return i >= 0 ? args[i + 1] : undefined;
};
const spec = opt('spec');
const out = opt('out');
const filter = opt('filter') ? new RegExp(opt('filter')) : null;

/** Spec file -> its exported suite function. */
const SUITES = {
  smoke: 'smoke',
  tsconfig: 'tsconfig',
  cli: 'cli',
  'commonjs-mode-contracts': 'commonJsModeContracts',
  'resolution-priority': 'resolutionPriority',
  'version-sensitive': 'versionSensitiveTests',
  loaders: 'loaders',
};

/** Tests the oracle can't compare, by what they exercise. Checked before a test runs. */
const SOURCE_SKIPS = [
  [/\btsxEsm\w*Path|\btsxCjs\w*Path/, "tsx-only: tsx's API (tsx/esm/api, tsx/cjs/api, register())"],
  [/ptyShell|\bshell\.type/, 'tsx-only: interactive pty shell'],
];
const NAME_SKIPS = [
  [/^CLI > (argv > )?(version|help)\b/, "tsx-only: tsx's --version / --help text"],
  [/^CLI > eval & print\b/, 'not in ts-r-run: --eval / --print (ts-r-run runs a file)'],
  [/\bwatch\b/i, "tsx-only: `tsx watch` (ts-r-run's --watch is a different CLI)"],
];

runner.selected = (name) => !filter || filter.test(name);
runner.skipReason = (name, fn) => {
  for (const [re, why] of NAME_SKIPS) if (re.test(name)) return why;
  const source = String(fn);
  for (const [re, why] of SOURCE_SKIPS) if (re.test(source)) return why;
  return null;
};

const file = path.resolve('tests/specs', `${spec}.ts`);
const mod = await import(pathToFileURL(file).href);
const suite = mod[SUITES[spec]];
if (typeof suite !== 'function') throw new Error(`no suite export in ${spec}`);
const node = await createNode(process.version);
try {
  await suite(node);
} catch (error) {
  runner.results.push({ name: spec, status: 'error', error: String(error?.stack ?? error) });
}
await settle();
fs.writeFileSync(out, JSON.stringify(runner.results, null, 1));
process.exit(0);
