// esbuild's side of the `rebuild-*` scenarios: one build through esbuild's incremental
// context API, then `runs` times append a comment to FILE and rebuild (FILE is restored
// afterwards). Mirrors `ts_r_bundler`'s `build_loop --edit`, with the same flags.
//
//   node bench/js/esbuild-rebuild.mjs <working-dir> <entry> <runs> <file> [--minify]
//     [--format=esm|cjs|iife] [--loader:.EXT=LOADER]...

import fs from 'node:fs';
import path from 'node:path';
import { createRequire } from 'node:module';

const require = createRequire(new URL('../package.json', import.meta.url));
const esbuild = require('esbuild');

const [wd, entry, runsArg, fileArg] = process.argv.slice(2);
const flags = process.argv.slice(6);
const minify = flags.includes('--minify');
const loader = {};
let format;
for (const f of flags) {
  const l = f.match(/^--loader:(\.[^=]+)=(.+)$/);
  if (l) loader[l[1]] = l[2];
  const m = f.match(/^--format=(.+)$/);
  if (m) format = m[1];
}
const runs = Number(runsArg);
const file = path.resolve(wd, fileArg);
const original = fs.readFileSync(file, 'utf8');
const ctx = await esbuild.context({
  absWorkingDir: path.resolve(wd),
  entryPoints: [entry],
  bundle: true,
  minify,
  format,
  loader,
  jsx: 'automatic',
  outfile: 'out.js',
  write: false,
  logLevel: 'error',
});
const times = [];
try {
  await ctx.rebuild();
  for (let i = 0; i < runs; i++) {
    fs.writeFileSync(file, `${original}\n// edit ${i}\n`);
    const t = process.hrtime.bigint();
    await ctx.rebuild();
    times.push(Number(process.hrtime.bigint() - t) / 1e6);
  }
} finally {
  fs.writeFileSync(file, original);
  await ctx.dispose();
}
times.sort((a, b) => a - b);
console.log(`runs ${runs}: min ${times[0].toFixed(1)}ms median ${times[Math.floor(runs / 2)].toFixed(1)}ms`);
